use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use anyhow::{Context, Result};

use super::proto::{recv_msg, send_msg, TestMsg, PROTOCOL_VERSION};
use super::report::Report;
use super::spawn::{bind_listener, make_socket_path, spawn, ChildSpec};
use super::TestArgs;

pub fn run(args: TestArgs) -> Result<()> {
    let mut report = Report::new();

    let vk = match super::vk::instance::create_instance() {
        Ok(v) => v,
        Err(e) => {
            report.fatal(format!("vkCreateInstance: {e}"));
            report.emit();
            std::process::exit(2);
        }
    };
    let devices = match super::vk::instance::enumerate(&vk) {
        Ok(d) => d,
        Err(e) => {
            report.fatal(format!("enumerate physical devices: {e}"));
            report.emit();
            std::process::exit(2);
        }
    };
    if devices.is_empty() {
        report.fatal("no Vulkan-capable devices found");
        report.emit();
        std::process::exit(2);
    }
    report.note_devices(&devices);

    let orch_idx = args
        .device_indices
        .first()
        .copied()
        .unwrap_or_else(|| pick_default(&devices));
    let child_idx = args.device_indices.get(1).copied().unwrap_or(orch_idx);
    for (label, idx) in [("orch", orch_idx), ("child", child_idx)] {
        if idx >= devices.len() {
            anyhow::bail!(
                "--test-gpus {label}={idx} out of range (have {})",
                devices.len()
            );
        }
    }
    let orch_dev = devices[orch_idx].clone();
    let child_dev = devices[child_idx].clone();
    report.note_picked_device(&orch_dev);
    if child_idx != orch_idx {
        report.note_child_device(&child_dev);
    }

    let vkd = match super::vk::device::create(&vk.instance, &orch_dev) {
        Ok(d) => d,
        Err(e) => {
            report.fatal(format!("vkCreateDevice on {}: {e}", orch_dev.name));
            report.emit();
            std::process::exit(2);
        }
    };

    let peer_sock = make_socket_path("peer")?;
    let (listener, _cleanup) = bind_listener(&peer_sock)?;
    let mut child = spawn(&ChildSpec {
        role: "peer",
        socket: peer_sock.clone(),
        vk_uuid: child_dev.uuid,
        slot: 0,
        display_name: None,
        instance_id: None,
        max_frames: None,
        capture_stdout: false,
    })?;

    let stream = match accept_with_timeout(&listener, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => {
            report.fatal(format!("peer never connected: {e}"));
            let _ = child.kill();
            report.emit();
            std::process::exit(2);
        }
    };

    if let Err(e) = do_handshake(&stream, &child_dev) {
        report.fatal(format!("handshake: {e}"));
        let _ = child.kill();
        report.emit();
        std::process::exit(2);
    }

    match super::tests::modifier_matrix::run_orchestrator(
        &vk.instance,
        orch_dev.phys,
        &vkd,
        &stream,
    ) {
        Ok(p) => report.modifier_matrix = Some(p),
        Err(e) => {
            log::warn!("modifier_matrix aborted: {e}");
            report.fatal(format!("modifier_matrix: {e}"));
        }
    }
    let _ = send_msg(&stream, &TestMsg::MatrixDone, &[]);
    let _ = recv_msg(&stream);

    let cross_gpu = orch_dev.uuid != child_dev.uuid;
    match super::tests::render_loop::run_orchestrator(
        &vk.instance,
        orch_dev.phys,
        &vkd,
        &stream,
        cross_gpu,
    ) {
        Ok(p) => report.render_loop = Some(p),
        Err(e) => log::warn!("render_loop aborted: {e}"),
    }

    let _ = send_msg(
        &stream,
        &TestMsg::Bye {
            reason: "test complete".into(),
        },
        &[],
    );
    drop(stream);
    let _ = child.wait();

    if !args.skip_fanout {
        match super::tests::fanout::run_orchestrator(
            &vk.instance,
            orch_dev.phys,
            &vkd,
            &child_dev,
            cross_gpu,
        ) {
            Ok(p) => report.fanout = Some(p),
            Err(e) => log::warn!("fanout aborted: {e}"),
        }
    }

    report.emit();
    Ok(())
}

fn accept_with_timeout(l: &UnixListener, timeout: Duration) -> Result<UnixStream> {
    l.set_nonblocking(true)?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match l.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false)?;
                return Ok(s);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    anyhow::bail!("accept timeout after {:?}", timeout);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn do_handshake(stream: &UnixStream, dev_meta: &super::vk::instance::DeviceMeta) -> Result<()> {
    let (msg, _fds) = recv_msg(stream).context("recv Hello")?;
    let TestMsg::Hello {
        version,
        device_uuid_hex,
        ..
    } = msg
    else {
        anyhow::bail!("expected Hello, got {msg:?}");
    };
    if version != PROTOCOL_VERSION {
        send_msg(
            stream,
            &TestMsg::Welcome {
                ok: false,
                message: format!(
                    "protocol version mismatch: orch={PROTOCOL_VERSION} child={version}"
                ),
            },
            &[],
        )
        .ok();
        anyhow::bail!("protocol version mismatch");
    }
    let want = super::format_uuid_hex(&dev_meta.uuid);
    if device_uuid_hex != want {
        send_msg(
            stream,
            &TestMsg::Welcome {
                ok: false,
                message: format!("vk uuid mismatch: orch={want} child={device_uuid_hex}"),
            },
            &[],
        )
        .ok();
        anyhow::bail!("child picked a different vk device");
    }
    send_msg(
        stream,
        &TestMsg::Welcome {
            ok: true,
            message: format!("orch picked {}", dev_meta.name),
        },
        &[],
    )?;
    Ok(())
}

fn pick_default(devs: &[super::vk::instance::DeviceMeta]) -> usize {
    use ash::vk::PhysicalDeviceType;
    let order = [
        PhysicalDeviceType::DISCRETE_GPU,
        PhysicalDeviceType::INTEGRATED_GPU,
        PhysicalDeviceType::VIRTUAL_GPU,
        PhysicalDeviceType::CPU,
        PhysicalDeviceType::OTHER,
    ];
    for kind in order {
        if let Some(i) = devs.iter().position(|d| d.kind == kind) {
            return i;
        }
    }
    0
}
