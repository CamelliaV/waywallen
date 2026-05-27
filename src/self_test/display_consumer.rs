use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ash::vk::{self, Handle};
use waywallen_display_sys as ffi;

use super::tests::render_loop::color_for;
use super::vk::cmd;
use super::vk::device::VkDevice;
use super::vk::image::{create_host_buffer, destroy_host_buffer, HostBuffer};
use super::vk::instance::find_by_uuid;
use super::TestArgs;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_TIMEOUT_MS: i32 = 5_000;

struct DisplayState {
    // VkDevice borrow held for the whole session; the FFI library
    // imports dma-bufs into this device, so its lifetime must outlive
    // every callback.
    vkd: *const VkDevice,

    // Latest textures pool from on_textures_ready; cleared on releasing.
    tex_count: u32,
    tex_w: u32,
    tex_h: u32,
    fourcc: u32,
    modifier: u64,
    vk_images: Vec<vk::Image>,

    cmdbuf: Option<cmd::OneShotCmd>,
    staging: Option<HostBuffer>,

    // counters
    frames_seen: u64,
    ok: u64,
    mismatch: u64,
    fatal: Option<String>,
    disconnected: bool,
    max_frames: u64,
}

pub fn run(args: TestArgs) -> Result<()> {
    let socket = args
        .socket
        .clone()
        .ok_or_else(|| anyhow!("--socket required"))?;
    let want_uuid = args.vk_uuid.ok_or_else(|| anyhow!("--vk-uuid required"))?;
    let display_name = args
        .display_name
        .clone()
        .unwrap_or_else(|| format!("self-test-display-{}", args.slot));
    let instance_id = args.instance_id.clone().unwrap_or_default();
    let max_frames = args.max_frames.unwrap_or(60).max(1);

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

    let result = run_session(
        &vk.instance,
        dev_meta.phys,
        &vkd,
        &socket,
        &display_name,
        &instance_id,
        max_frames,
        args.slot,
    );

    // Always emit a final status line — orchestrator parses stdout.
    if let Err(ref e) = result {
        emit_status(args.slot, 0, 0, 0, false, Some(format!("{e:#}")));
    }
    result.map(|_| ())
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    instance: &ash::Instance,
    phys: vk::PhysicalDevice,
    vkd: &VkDevice,
    socket: &PathBuf,
    display_name: &str,
    instance_id: &str,
    max_frames: u64,
    slot: u32,
) -> Result<()> {
    let mut state = Box::new(DisplayState {
        vkd: vkd as *const _,
        tex_count: 0,
        tex_w: 0,
        tex_h: 0,
        fourcc: 0,
        modifier: 0,
        vk_images: Vec::new(),
        cmdbuf: None,
        staging: None,
        frames_seen: 0,
        ok: 0,
        mismatch: 0,
        fatal: None,
        disconnected: false,
        max_frames,
    });
    let state_ptr: *mut DisplayState = state.as_mut() as *mut _;

    let cb = ffi::waywallen_display_callbacks_t {
        on_textures_ready: Some(cb_textures_ready),
        on_textures_releasing: Some(cb_textures_releasing),
        on_config: Some(cb_config),
        on_frame_ready: Some(cb_frame_ready),
        on_disconnected: Some(cb_disconnected),
        user_data: state_ptr as *mut c_void,
    };

    let d = unsafe { ffi::waywallen_display_new(&cb) };
    if d.is_null() {
        anyhow::bail!("waywallen_display_new returned null");
    }
    let _drop_d = FfiHandleGuard(d);

    let vk_ctx = ffi::waywallen_vk_ctx_t {
        instance: instance.handle().as_raw() as *mut c_void,
        physical_device: phys.as_raw() as *mut c_void,
        device: vkd.device.handle().as_raw() as *mut c_void,
        queue_family_index: vkd.queue_family,
        // FFI dlopens its own libvulkan and resolves entrypoints there;
        // setting None lets the library use its internal loader. Our
        // ash-loaded entrypoints share the same VkInstance handle, so
        // imports use the same driver.
        vk_get_instance_proc_addr: None,
    };
    let rc = unsafe { ffi::waywallen_display_bind_vulkan(d, &vk_ctx) };
    if rc != ffi::WAYWALLEN_OK {
        anyhow::bail!("waywallen_display_bind_vulkan rc={rc}");
    }

    let sock_c = CString::new(socket.to_string_lossy().as_bytes()).context("socket path nul")?;
    let name_c = CString::new(display_name.as_bytes()).context("display name nul")?;
    let inst_c = CString::new(instance_id.as_bytes()).context("instance id nul")?;
    let rc = unsafe {
        ffi::waywallen_display_begin_connect(
            d,
            sock_c.as_ptr(),
            name_c.as_ptr(),
            inst_c.as_ptr(),
            1920,
            1080,
            60_000,
        )
    };
    if rc != ffi::WAYWALLEN_OK {
        anyhow::bail!("begin_connect rc={rc}");
    }

    let fd = unsafe { ffi::waywallen_display_get_fd(d) };
    if fd < 0 {
        anyhow::bail!("waywallen_display_get_fd returned {fd}");
    }
    advance_handshake(d, fd)?;

    log::info!("display_consumer[{slot}]: connected, draining events");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !state.disconnected
        && state.frames_seen < state.max_frames
        && state.fatal.is_none()
        && Instant::now() < deadline
    {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let pr = unsafe { libc::poll(&mut pfd, 1, POLL_TIMEOUT_MS) };
        if pr < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            anyhow::bail!("poll: {e}");
        }
        if pr == 0 {
            // idle tick — re-evaluate loop conditions
            continue;
        }
        if (pfd.revents & (libc::POLLERR | libc::POLLHUP)) != 0 {
            log::info!("display_consumer[{slot}]: POLLERR/POLLHUP, draining once and exit");
            let _ = unsafe { ffi::waywallen_display_dispatch(d) };
            break;
        }
        if (pfd.revents & libc::POLLIN) != 0 {
            let r = unsafe { ffi::waywallen_display_dispatch(d) };
            if r < 0 {
                break;
            }
        }
    }

    let clean =
        state.fatal.is_none() && (state.frames_seen >= state.max_frames || state.disconnected);
    emit_status(
        slot,
        state.frames_seen,
        state.ok,
        state.mismatch,
        clean,
        state.fatal.clone(),
    );

    // Tear down GPU resources we allocated lazily.
    teardown(vkd, &mut state);
    drop(state);
    Ok(())
}

fn emit_status(
    slot: u32,
    frames: u64,
    ok: u64,
    mismatch: u64,
    clean_exit: bool,
    fatal: Option<String>,
) {
    println!(
        "{{\"role\":\"display\",\"slot\":{slot},\"frames\":{frames},\"ok\":{ok},\"mismatch\":{mismatch},\"clean_exit\":{clean_exit},\"fatal\":{}}}",
        match fatal.as_deref() {
            Some(s) => format!("{:?}", s),
            None => "null".into(),
        }
    );
}

fn advance_handshake(d: *mut ffi::waywallen_display_t, fd: c_int) -> Result<()> {
    loop {
        let r = unsafe { ffi::waywallen_display_advance_handshake(d) };
        if r == ffi::WAYWALLEN_HS_DONE {
            return Ok(());
        }
        if r < 0 {
            anyhow::bail!("advance_handshake rc={r}");
        }
        let events = match r {
            ffi::WAYWALLEN_HS_NEED_READ => libc::POLLIN,
            ffi::WAYWALLEN_HS_NEED_WRITE => libc::POLLOUT,
            _ => libc::POLLIN | libc::POLLOUT,
        };
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let deadline_ms = CONNECT_TIMEOUT.as_millis() as i32;
        let pr = unsafe { libc::poll(&mut pfd, 1, deadline_ms) };
        if pr < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            anyhow::bail!("poll(handshake): {e}");
        }
        if pr == 0 {
            anyhow::bail!("handshake timed out after {:?}", CONNECT_TIMEOUT);
        }
    }
}

fn ensure_gpu_resources(state: &mut DisplayState) -> Result<()> {
    if state.cmdbuf.is_some() && state.staging.is_some() {
        return Ok(());
    }
    let vkd = unsafe { &*state.vkd };
    if state.cmdbuf.is_none() {
        state.cmdbuf = Some(cmd::create(vkd)?);
    }
    if state.staging.is_none() {
        let size = (state.tex_w * state.tex_h * 4) as u64;
        state.staging = Some(create_host_buffer(vkd, size)?);
    }
    Ok(())
}

fn validate_frame(state: &mut DisplayState, buffer_index: u32, seq: u64) -> Result<bool> {
    if (buffer_index as usize) >= state.vk_images.len() {
        anyhow::bail!(
            "frame buffer_index {buffer_index} out of range (have {})",
            state.vk_images.len()
        );
    }
    ensure_gpu_resources(state)?;
    let vkd = unsafe { &*state.vkd };
    let cmdbuf = state.cmdbuf.as_ref().unwrap();
    let staging = state.staging.as_ref().unwrap();
    let src = state.vk_images[buffer_index as usize];

    unsafe {
        vkd.device
            .reset_command_buffer(cmdbuf.buf, vk::CommandBufferResetFlags::empty())?;
        vkd.device.begin_command_buffer(
            cmdbuf.buf,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;

        // Imported image is in GENERAL after the producer's clear.
        let region = vk::BufferImageCopy::default()
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .image_extent(vk::Extent3D {
                width: state.tex_w,
                height: state.tex_h,
                depth: 1,
            });
        vkd.device.cmd_copy_image_to_buffer(
            cmdbuf.buf,
            src,
            vk::ImageLayout::GENERAL,
            staging.buffer,
            &[region],
        );
        vkd.device.end_command_buffer(cmdbuf.buf)?;

        let bufs = [cmdbuf.buf];
        vkd.device.queue_submit(
            vkd.queue,
            &[vk::SubmitInfo::default().command_buffers(&bufs)],
            vk::Fence::null(),
        )?;
        vkd.device.queue_wait_idle(vkd.queue)?;
    }

    let seq32 = (seq & 0xFFFF_FFFF) as u32;
    let (_, expected) = color_for(seq32);
    let pixel = unsafe {
        // Read pixel (0,0). DMA-BUF imported with modifier may have
        // tile-shape, so picking (0,0) is the most modifier-tolerant
        // canary — the producer cleared the entire image to one
        // colour, every pixel must equal `expected`.
        let p = staging.mapped;
        [*p, *p.add(1), *p.add(2), *p.add(3)]
    };
    Ok(pixel == expected)
}

fn teardown(vkd: &VkDevice, state: &mut DisplayState) {
    unsafe {
        let _ = vkd.device.device_wait_idle();
    }
    if let Some(c) = state.cmdbuf.take() {
        cmd::destroy(vkd, c);
    }
    if let Some(s) = state.staging.take() {
        destroy_host_buffer(vkd, s);
    }
}

// ---------------------------------------------------------------------------
// FFI callbacks — every entry traps panics so a Rust panic never unwinds
// across the C boundary.
// ---------------------------------------------------------------------------

unsafe extern "C" fn cb_textures_ready(
    user_data: *mut c_void,
    t: *const ffi::waywallen_textures_t,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let state = &mut *(user_data as *mut DisplayState);
        let t = &*t;
        state.tex_count = t.count;
        state.tex_w = t.tex_width;
        state.tex_h = t.tex_height;
        state.fourcc = t.fourcc;
        state.modifier = t.modifier;
        // Backend should be VULKAN since we bound a Vulkan ctx.
        if t.backend != ffi::WAYWALLEN_BACKEND_VULKAN {
            state.fatal = Some(format!(
                "expected VULKAN backend, got {} on textures_ready",
                t.backend
            ));
            return;
        }
        state.vk_images.clear();
        if !t.vk_images.is_null() {
            for i in 0..t.count {
                let raw = *t.vk_images.add(i as usize);
                let h = vk::Image::from_raw(raw as u64);
                state.vk_images.push(h);
            }
        }
        // Staging dims may have changed — drop and re-alloc lazily.
        if let Some(s) = state.staging.take() {
            let vkd = &*state.vkd;
            destroy_host_buffer(vkd, s);
        }
    }));
}

unsafe extern "C" fn cb_textures_releasing(
    user_data: *mut c_void,
    _t: *const ffi::waywallen_textures_t,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let state = &mut *(user_data as *mut DisplayState);
        state.vk_images.clear();
        state.tex_count = 0;
    }));
}

unsafe extern "C" fn cb_config(_user_data: *mut c_void, _c: *const ffi::waywallen_config_t) {
    // self_test ignores config geometry — orchestrator runs at a single
    // size and cares about frame fidelity only.
}

unsafe extern "C" fn cb_frame_ready(user_data: *mut c_void, f: *const ffi::waywallen_frame_t) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let state = &mut *(user_data as *mut DisplayState);
        let f = &*f;
        state.frames_seen += 1;
        match validate_frame(state, f.buffer_index, f.seq) {
            Ok(true) => state.ok += 1,
            Ok(false) => state.mismatch += 1,
            Err(e) => {
                state.fatal = Some(format!(
                    "validate frame seq={} idx={}: {e:#}",
                    f.seq, f.buffer_index
                ));
            }
        }
        if f.release_syncobj_fd >= 0 {
            let _ = ffi::waywallen_display_signal_release_syncobj(f.release_syncobj_fd);
        }
    }));
}

unsafe extern "C" fn cb_disconnected(user_data: *mut c_void, code: c_int, msg: *const c_char) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let state = &mut *(user_data as *mut DisplayState);
        state.disconnected = true;
        let msg_str = if msg.is_null() {
            String::new()
        } else {
            CStr::from_ptr(msg).to_string_lossy().into_owned()
        };
        log::info!("display_consumer: disconnected code={code} msg={msg_str:?}");
    }));
}

struct FfiHandleGuard(*mut ffi::waywallen_display_t);
impl Drop for FfiHandleGuard {
    fn drop(&mut self) {
        // Same-thread tear-down: close + drain + free.
        unsafe { ffi::waywallen_display_shutdown(self.0) };
    }
}
