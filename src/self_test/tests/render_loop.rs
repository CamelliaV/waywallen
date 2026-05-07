use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use anyhow::{anyhow, Context, Result};
use ash::vk;

use super::super::proto::{recv_msg, send_msg, TestMsg};
use super::super::report::RenderLoop;
use super::super::vk::cmd;
use super::super::vk::device::VkDevice;
use super::super::vk::image::{
    create_host_buffer, create_with_modifiers, destroy_host_buffer, export_dmabuf,
    import_dmabuf, HostBuffer,
};
use super::super::vk::modifier::format_modifier;
use super::super::vk::sync::{
    create_binary_importable, create_binary_sync_fd_exportable, create_timeline_exportable,
    export_opaque_fd, export_signaled_sync_fd, import_sync_fd_temporary,
    import_timeline_opaque_fd, wait_timeline, TimelineSemaphore,
};

const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const FOURCC_AB24: u32 = 0x34324241;
const WIDTH: u32 = 256;
const HEIGHT: u32 = 256;
const FRAMES: u32 = 240;
const PER_FRAME_TIMEOUT_NS: u64 = 1_000_000_000;

pub fn color_for(n: u32) -> ([f32; 4], [u8; 4]) {
    let r = (n & 0xFF) as u8;
    let g = ((n >> 1) & 0xFF) as u8;
    let b = ((n >> 2) & 0xFF) as u8;
    let a = 0xFFu8;
    let f = [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ];
    (f, [r, g, b, a])
}

fn pack_rgba(c: [u8; 4]) -> u32 {
    ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | (c[3] as u32)
}

#[cfg(test)]
fn unpack_rgba(v: u32) -> [u8; 4] {
    [
        ((v >> 24) & 0xFF) as u8,
        ((v >> 16) & 0xFF) as u8,
        ((v >> 8) & 0xFF) as u8,
        (v & 0xFF) as u8,
    ]
}

fn pick_modifier(
    vkd: &VkDevice,
    instance: &ash::Instance,
    phys: vk::PhysicalDevice,
    cross_gpu: bool,
) -> Result<u64> {
    // LINEAR is the only modifier guaranteed importable across vendors —
    // tiled layouts encode a vendor-specific tile shape the consumer
    // GPU cannot decode. Skip the picker entirely on the cross-GPU path.
    if cross_gpu {
        return Ok(0);
    }
    let entries = super::super::vk::modifier::query_supported(instance, phys, FORMAT)?;
    let _ = vkd;
    if let Some(e) = entries
        .iter()
        .find(|e| e.modifier != 0 && super::super::vk::modifier::supports_clear_and_export(e))
    {
        return Ok(e.modifier);
    }
    Ok(0)
}

pub fn run_orchestrator(
    instance: &ash::Instance,
    phys: vk::PhysicalDevice,
    vkd: &VkDevice,
    sock: &UnixStream,
    cross_gpu: bool,
) -> Result<RenderLoop> {
    let modifier = pick_modifier(vkd, instance, phys, cross_gpu)?;
    log::info!(
        "render_loop: using modifier {:#x} ({})",
        modifier,
        format_modifier(modifier)
    );

    let img0 = create_with_modifiers(
        vkd,
        WIDTH,
        HEIGHT,
        FORMAT,
        vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        &[modifier],
        cross_gpu,
    )
    .context("alloc slot 0")?;
    let img1 = create_with_modifiers(
        vkd,
        WIDTH,
        HEIGHT,
        FORMAT,
        vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
        &[modifier],
        cross_gpu,
    )
    .context("alloc slot 1")?;

    let cmdbuf = cmd::create(vkd)?;
    cmd::transition_to_general(vkd, &cmdbuf, &[img0.image, img1.image])
        .context("transition slots to GENERAL")?;

    let use_timelines = !cross_gpu;
    let timelines = if use_timelines {
        Some((
            create_timeline_exportable(vkd).context("create acquire timeline")?,
            create_timeline_exportable(vkd).context("create release timeline")?,
        ))
    } else {
        None
    };
    // Cross-GPU per-frame SYNC_FD: amdgpu won't schedule a foreign
    // dma-buf without an explicit dma_fence wait on the consumer's
    // submit. Producer signals into this each frame, exports SYNC_FD,
    // ships it with Frame, and the SYNC_FD is consumed on temporary
    // import for the consumer's wait.
    let sync_sem = if cross_gpu {
        Some(create_binary_sync_fd_exportable(vkd).context("create sync_fd sem")?)
    } else {
        None
    };

    let fd0 = export_dmabuf(vkd, &img0).context("export slot 0 dma-buf")?;
    let fd1 = export_dmabuf(vkd, &img1).context("export slot 1 dma-buf")?;
    send_msg(
        sock,
        &TestMsg::BindPair {
            fourcc: FOURCC_AB24,
            modifier: img0.modifier,
            width: WIDTH,
            height: HEIGHT,
            slot_strides: [
                u32::try_from(img0.plane0_stride).unwrap_or(u32::MAX),
                u32::try_from(img1.plane0_stride).unwrap_or(u32::MAX),
            ],
            slot_offsets: [
                u32::try_from(img0.plane0_offset).unwrap_or(0),
                u32::try_from(img1.plane0_offset).unwrap_or(0),
            ],
            slot_sizes: [img0.plane0_size, img1.plane0_size],
            color_seed: 0,
            frame_count: FRAMES,
            use_timelines,
        },
        &[fd0.as_raw_fd(), fd1.as_raw_fd()],
    )
    .map_err(|e| anyhow!("send BindPair: {e}"))?;
    drop((fd0, fd1));

    if let Some((acquire, release)) = &timelines {
        let acq_fd = export_opaque_fd(vkd, acquire).context("export acquire fd")?;
        let rel_fd = export_opaque_fd(vkd, release).context("export release fd")?;
        send_msg(
            sock,
            &TestMsg::BindTimelines,
            &[acq_fd.as_raw_fd(), rel_fd.as_raw_fd()],
        )
        .map_err(|e| anyhow!("send BindTimelines: {e}"))?;
        drop((acq_fd, rel_fd));
    }

    let imgs = [img0.image, img1.image];
    let mut report = RenderLoop {
        frames: FRAMES,
        ok: 0,
        color_mismatch: 0,
        acquire_timeout: 0,
        modifier_used: modifier,
        modifier_name: format_modifier(modifier),
    };

    for n in 0..FRAMES {
        let slot = (n & 1) as usize;
        let acq_val = (n + 1) as u64;
        let rel_val = (n + 1) as u64;
        let (color_f, _) = color_for(n);

        unsafe {
            vkd.device
                .reset_command_buffer(cmdbuf.buf, vk::CommandBufferResetFlags::empty())?;
            vkd.device.begin_command_buffer(
                cmdbuf.buf,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            vkd.device.cmd_clear_color_image(
                cmdbuf.buf,
                imgs[slot],
                vk::ImageLayout::GENERAL,
                &vk::ClearColorValue { float32: color_f },
                &[vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1)],
            );
            vkd.device.end_command_buffer(cmdbuf.buf)?;

            let bufs = [cmdbuf.buf];
            if let Some((acquire, _)) = &timelines {
                let signal_sems = [acquire.sem];
                let signal_vals = [acq_val];
                let mut tl_submit = vk::TimelineSemaphoreSubmitInfo::default()
                    .signal_semaphore_values(&signal_vals);
                vkd.device.queue_submit(
                    vkd.queue,
                    &[vk::SubmitInfo::default()
                        .command_buffers(&bufs)
                        .signal_semaphores(&signal_sems)
                        .push_next(&mut tl_submit)],
                    vk::Fence::null(),
                )?;
            } else if let Some(sem) = sync_sem {
                let signal_sems = [sem];
                vkd.device.queue_submit(
                    vkd.queue,
                    &[vk::SubmitInfo::default()
                        .command_buffers(&bufs)
                        .signal_semaphores(&signal_sems)],
                    vk::Fence::null(),
                )?;
            } else {
                vkd.device.queue_submit(
                    vkd.queue,
                    &[vk::SubmitInfo::default().command_buffers(&bufs)],
                    vk::Fence::null(),
                )?;
                vkd.device.queue_wait_idle(vkd.queue)?;
            }
        }

        if let Some(sem) = sync_sem {
            let fd = export_signaled_sync_fd(vkd, sem)
                .with_context(|| format!("export sync_fd n={n}"))?;
            let raw = fd.as_raw_fd();
            send_msg(
                sock,
                &TestMsg::Frame {
                    n,
                    slot: slot as u32,
                    acquire_value: acq_val,
                    release_value: rel_val,
                },
                &[raw],
            )
            .map_err(|e| anyhow!("send Frame n={n}: {e}"))?;
            drop(fd);
        } else {
            send_msg(
                sock,
                &TestMsg::Frame {
                    n,
                    slot: slot as u32,
                    acquire_value: acq_val,
                    release_value: rel_val,
                },
                &[],
            )
            .map_err(|e| anyhow!("send Frame n={n}: {e}"))?;
        }

        if let Some((_, release)) = &timelines {
            match wait_timeline(vkd, release, rel_val, PER_FRAME_TIMEOUT_NS) {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("render_loop: release timeout at frame {n}: {e}");
                    report.acquire_timeout += 1;
                    // First timeout poisons the rest of the run; bail loudly.
                    break;
                }
            }
        }

        let (msg, _) = recv_msg(sock).map_err(|e| anyhow!("recv ColorReport n={n}: {e}"))?;
        match msg {
            TestMsg::ColorReport {
                n: rn,
                ok,
                ..
            } => {
                if rn != n {
                    log::warn!("render_loop: out-of-order ColorReport: n={rn} expected {n}");
                }
                if ok {
                    report.ok += 1;
                } else {
                    report.color_mismatch += 1;
                }
            }
            other => anyhow::bail!("expected ColorReport got {other:?}"),
        }
    }

    let _ = send_msg(sock, &TestMsg::LoopDone, &[]);
    let _ = recv_msg(sock);

    unsafe {
        let _ = vkd.device.device_wait_idle();
        if let Some((acquire, release)) = &timelines {
            vkd.device.destroy_semaphore(acquire.sem, None);
            vkd.device.destroy_semaphore(release.sem, None);
        }
        if let Some(sem) = sync_sem {
            vkd.device.destroy_semaphore(sem, None);
        }
        vkd.device.free_memory(img0.memory, None);
        vkd.device.free_memory(img1.memory, None);
        vkd.device.destroy_image(img0.image, None);
        vkd.device.destroy_image(img1.image, None);
    }
    cmd::destroy(vkd, cmdbuf);

    Ok(report)
}

pub fn run_peer(vkd: &VkDevice, sock: &UnixStream) -> Result<()> {
    let (msg, fds) = recv_msg(sock).map_err(|e| anyhow!("recv BindPair: {e}"))?;
    let TestMsg::BindPair {
        fourcc: _,
        modifier,
        width,
        height,
        slot_strides,
        slot_offsets,
        slot_sizes: _,
        color_seed: _,
        frame_count,
        use_timelines,
    } = msg
    else {
        anyhow::bail!("expected BindPair, got {msg:?}");
    };
    if fds.len() != 2 {
        anyhow::bail!("BindPair: expected 2 fds, got {}", fds.len());
    }
    let mut fds = fds.into_iter();
    let fd0 = fds.next().unwrap();
    let fd1 = fds.next().unwrap();
    let img0 = import_dmabuf(
        vkd,
        fd0,
        width,
        height,
        FORMAT,
        vk::ImageUsageFlags::TRANSFER_SRC,
        modifier,
        slot_strides[0] as u64,
        slot_offsets[0] as u64,
    )
    .context("import slot 0")?;
    let img1 = import_dmabuf(
        vkd,
        fd1,
        width,
        height,
        FORMAT,
        vk::ImageUsageFlags::TRANSFER_SRC,
        modifier,
        slot_strides[1] as u64,
        slot_offsets[1] as u64,
    )
    .context("import slot 1")?;

    let timelines = if use_timelines {
        let (msg, fds) = recv_msg(sock).map_err(|e| anyhow!("recv BindTimelines: {e}"))?;
        if !matches!(msg, TestMsg::BindTimelines) {
            anyhow::bail!("expected BindTimelines got {msg:?}");
        }
        if fds.len() != 2 {
            anyhow::bail!("BindTimelines: expected 2 fds, got {}", fds.len());
        }
        let mut fds = fds.into_iter();
        let acq_fd = fds.next().unwrap();
        let rel_fd = fds.next().unwrap();
        let acquire = import_timeline_opaque_fd(vkd, acq_fd).context("import acquire timeline")?;
        let release = import_timeline_opaque_fd(vkd, rel_fd).context("import release timeline")?;
        Some((acquire, release))
    } else {
        None
    };

    let cmdbuf = cmd::create(vkd)?;
    let (read_layout, peer_sync_sem) = if use_timelines {
        cmd::transition_to_general(vkd, &cmdbuf, &[img0.image, img1.image])
            .context("peer transition imports")?;
        (vk::ImageLayout::GENERAL, vk::Semaphore::null())
    } else {
        // Cross-GPU: skip the upfront transition (an upfront submit
        // referencing the foreign BO fails on amdgpu before producer
        // pages exist) and treat the image as GENERAL on first read,
        // matching display_consumer.rs's working fanout pattern. The
        // per-frame SYNC_FD wait carries the producer's last-write
        // dependency through dma_fence — amdgpu won't schedule the
        // foreign BO without it.
        let sem = create_binary_importable(vkd).context("create peer sync_fd sem")?;
        (vk::ImageLayout::GENERAL, sem)
    };

    let staging = create_host_buffer(vkd, (WIDTH * HEIGHT * 4) as u64).context("alloc staging")?;
    let imgs = [img0.image, img1.image];

    for _ in 0..frame_count {
        let (msg, fds) = recv_msg(sock).map_err(|e| anyhow!("recv Frame: {e}"))?;
        match msg {
            TestMsg::Frame {
                n,
                slot,
                acquire_value,
                release_value,
            } => {
                let sync_wait = if peer_sync_sem != vk::Semaphore::null() {
                    let mut it = fds.into_iter();
                    let fd = it.next().ok_or_else(|| anyhow!("Frame missing sync_fd"))?;
                    import_sync_fd_temporary(vkd, peer_sync_sem, fd)
                        .with_context(|| format!("import sync_fd n={n}"))?;
                    Some(peer_sync_sem)
                } else {
                    None
                };
                run_one_frame(
                    vkd,
                    sock,
                    &cmdbuf,
                    timelines.as_ref(),
                    sync_wait,
                    &staging,
                    &imgs,
                    read_layout,
                    n,
                    slot,
                    acquire_value,
                    release_value,
                )?;
            }
            TestMsg::LoopDone => {
                send_msg(sock, &TestMsg::Ack, &[])
                    .map_err(|e| anyhow!("send Ack: {e}"))?;
                break;
            }
            other => anyhow::bail!("unexpected {other:?}"),
        }
    }

    let _ = send_msg(sock, &TestMsg::Ack, &[]);

    unsafe {
        let _ = vkd.device.device_wait_idle();
        if let Some((acquire, release)) = &timelines {
            vkd.device.destroy_semaphore(acquire.sem, None);
            vkd.device.destroy_semaphore(release.sem, None);
        }
        if peer_sync_sem != vk::Semaphore::null() {
            vkd.device.destroy_semaphore(peer_sync_sem, None);
        }
    }
    destroy_host_buffer(vkd, staging);
    cmd::destroy(vkd, cmdbuf);
    let _ = (img0, img1);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_one_frame(
    vkd: &VkDevice,
    sock: &UnixStream,
    cmdbuf: &cmd::OneShotCmd,
    timelines: Option<&(TimelineSemaphore, TimelineSemaphore)>,
    sync_wait: Option<vk::Semaphore>,
    staging: &HostBuffer,
    imgs: &[vk::Image; 2],
    read_layout: vk::ImageLayout,
    n: u32,
    slot: u32,
    acq_val: u64,
    rel_val: u64,
) -> Result<()> {
    if (slot as usize) >= imgs.len() {
        anyhow::bail!("frame slot {slot} out of range");
    }
    let src = imgs[slot as usize];

    unsafe {
        vkd.device
            .reset_command_buffer(cmdbuf.buf, vk::CommandBufferResetFlags::empty())?;
        vkd.device.begin_command_buffer(
            cmdbuf.buf,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        vkd.device.cmd_copy_image_to_buffer(
            cmdbuf.buf,
            src,
            read_layout,
            staging.buffer,
            &[vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(WIDTH)
                .buffer_image_height(HEIGHT)
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_offset(vk::Offset3D::default())
                .image_extent(vk::Extent3D {
                    width: WIDTH,
                    height: HEIGHT,
                    depth: 1,
                })],
        );
        vkd.device.end_command_buffer(cmdbuf.buf)?;

        let bufs = [cmdbuf.buf];
        if let Some((acquire, release)) = timelines {
            let wait_sems = [acquire.sem];
            let wait_vals = [acq_val];
            let signal_sems = [release.sem];
            let signal_vals = [rel_val];
            let mut tl_submit = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_vals)
                .signal_semaphore_values(&signal_vals);
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            vkd.device.queue_submit(
                vkd.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait_sems)
                    .wait_dst_stage_mask(&wait_stages)
                    .signal_semaphores(&signal_sems)
                    .command_buffers(&bufs)
                    .push_next(&mut tl_submit)],
                vk::Fence::null(),
            )?;
        } else if let Some(sem) = sync_wait {
            let wait_sems = [sem];
            let wait_stages = [vk::PipelineStageFlags::TRANSFER];
            vkd.device.queue_submit(
                vkd.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&wait_sems)
                    .wait_dst_stage_mask(&wait_stages)
                    .command_buffers(&bufs)],
                vk::Fence::null(),
            )?;
        } else {
            vkd.device.queue_submit(
                vkd.queue,
                &[vk::SubmitInfo::default().command_buffers(&bufs)],
                vk::Fence::null(),
            )?;
        }
    }

    if let Some((_, release)) = timelines {
        wait_timeline(vkd, release, rel_val, PER_FRAME_TIMEOUT_NS)
            .with_context(|| format!("peer wait release n={n}"))?;
    } else {
        unsafe {
            vkd.device
                .queue_wait_idle(vkd.queue)
                .with_context(|| format!("peer queue_wait_idle n={n}"))?;
        }
    }
    let _ = (acq_val, rel_val);

    let center_byte = ((HEIGHT / 2) * WIDTH + WIDTH / 2) as usize * 4;
    let got = unsafe {
        let p = staging.mapped.add(center_byte);
        [*p, *p.add(1), *p.add(2), *p.add(3)]
    };
    let (_, expected) = color_for(n);
    let ok = pixel_close_enough(got, expected);
    let report_msg = TestMsg::ColorReport {
        n,
        slot,
        expected_rgba: pack_rgba(expected),
        got_rgba: pack_rgba(got),
        ok,
    };
    send_msg(sock, &report_msg, &[]).map_err(|e| anyhow!("send ColorReport: {e}"))?;
    Ok(())
}

fn pixel_close_enough(got: [u8; 4], expected: [u8; 4]) -> bool {
    // ±1 LSB: UNORM rounding drifts by at most one count after float→u8.
    got.iter()
        .zip(expected.iter())
        .all(|(g, e)| (*g as i32 - *e as i32).abs() <= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_inverse() {
        for n in 0u32..32 {
            let (_, c) = color_for(n);
            let packed = pack_rgba(c);
            let back = unpack_rgba(packed);
            assert_eq!(c, back);
        }
    }

    #[test]
    fn pixel_close_enough_tolerates_lsb() {
        assert!(pixel_close_enough([10, 20, 30, 255], [10, 20, 30, 255]));
        assert!(pixel_close_enough([11, 19, 30, 254], [10, 20, 30, 255]));
        assert!(!pixel_close_enough([12, 20, 30, 255], [10, 20, 30, 255]));
    }
}
