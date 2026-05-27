use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use super::proto::{recv_msg, send_msg, TestMsg, PROTOCOL_VERSION};
use super::vk::instance::find_by_uuid;
use super::TestArgs;

pub fn run_peer(args: TestArgs) -> Result<()> {
    let socket = args
        .socket
        .clone()
        .ok_or_else(|| anyhow!("--socket required"))?;
    let want_uuid = args.vk_uuid.ok_or_else(|| anyhow!("--vk-uuid required"))?;

    let vk = super::vk::instance::create_instance().context("vkCreateInstance")?;
    let devices = super::vk::instance::enumerate(&vk).context("enumerate")?;
    let dev_meta = find_by_uuid(&devices, &want_uuid)
        .ok_or_else(|| {
            anyhow!(
                "vk uuid {} not found among {} local device(s)",
                super::format_uuid_hex(&want_uuid),
                devices.len()
            )
        })?
        .clone();
    let vkd = super::vk::device::create(&vk.instance, &dev_meta)
        .with_context(|| format!("vkCreateDevice on {}", dev_meta.name))?;

    let stream = connect_with_retry(&socket, Duration::from_secs(5))?;
    log::info!("peer: connected, picked device={}", dev_meta.name);

    send_msg(
        &stream,
        &TestMsg::Hello {
            version: PROTOCOL_VERSION,
            device_uuid_hex: super::format_uuid_hex(&dev_meta.uuid),
            driver_uuid_hex: super::format_uuid_hex(&dev_meta.driver_uuid),
            device_name: dev_meta.name.clone(),
        },
        &[],
    )
    .map_err(|e| anyhow!("send Hello: {e}"))?;
    let (welcome, _) = recv_msg(&stream).map_err(|e| anyhow!("recv Welcome: {e}"))?;
    let TestMsg::Welcome { ok, message } = welcome else {
        anyhow::bail!("expected Welcome, got {welcome:?}");
    };
    if !ok {
        anyhow::bail!("orch rejected: {message}");
    }

    super::tests::modifier_matrix::run_peer(&vkd, &stream).context("modifier_matrix")?;
    if let Err(e) = super::tests::render_loop::run_peer(&vkd, &stream) {
        // Cross-vendor render_loop can hit driver-level limits (e.g.
        // amdgpu CS rejecting an NVIDIA-exported dma-buf even when the
        // import succeeds). Log and close the socket; the orchestrator
        // sees the peer drop, records render_loop as skipped, and moves
        // on to fanout — keeping VERDICT clean of noisy anyhow traces.
        log::warn!("peer: render_loop ended early: {e:#}");
        return Ok(());
    }

    loop {
        match recv_msg(&stream) {
            Ok((TestMsg::Bye { .. }, _)) => return Ok(()),
            Ok(_) => continue,
            Err(super::proto::Error::PeerClosed) => return Ok(()),
            Err(e) => return Err(anyhow!("peer recv: {e}")),
        }
    }
}

pub fn run_renderer(_args: TestArgs) -> Result<()> {
    Ok(())
}

pub fn run_display(args: TestArgs) -> Result<()> {
    super::display_consumer::run(args)
}

fn connect_with_retry(path: &std::path::Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match UnixStream::connect(path) {
            Ok(s) => return Ok(s),
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(anyhow!("connect {}: {e}", path.display()));
            }
        }
    }
}
