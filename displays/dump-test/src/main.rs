//! `waywallen-dump-display` — headless test consumer for the
//! `waywallen-display-v3` protocol.
//!
//! Purpose: exercise the daemon's DMA-BUF sharing path end-to-end
//! without depending on a real Wayland compositor. Used by the
//! `dmabuf_roundtrip_e2e` integration test.
//!
//! Two modes:
//!
//! 1. `--print-caps` — query the local Vulkan stack for supported
//!    `(fourcc, modifier)` pairs and emit a `PeerCaps`-shaped JSON
//!    document on stdout, then exit. The test orchestrator parses
//!    this and intersects it with the renderer's caps to drive
//!    iteration.
//!
//! 2. Default (consumer) mode — connect to `--socket`, walk the
//!    handshake, advertise exactly the `(fourcc, modifier)` pairs
//!    given by `--advertise`, accept whichever pair the daemon
//!    picks, and for each `frame_ready`:
//!      a. import the DMA-BUF into Vulkan,
//!      b. blit/copy it back to a HOST_VISIBLE staging buffer,
//!      c. dump the linear bytes to `<dump-dir>/consumer-…bin` plus
//!         a `.json` sidecar with width/height/stride/fourcc/modifier,
//!      d. signal the release_syncobj fd so the producer can reuse
//!         the slot.
//!    Exits 0 after `--frames` frames or on `Bye` from the daemon.
//!
//! The Vulkan import + readback path lives in `vk_consumer.rs`.

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use waywallen::display_proto::{codec, Event, Request, PROTOCOL_NAME, PROTOCOL_VERSION};
use waywallen::sync::DrmDevice;

mod vk_consumer;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Cli {
    socket: Option<PathBuf>,
    advertise: Vec<(u32, u64)>,
    dump_dir: Option<PathBuf>,
    frames: u32,
    print_caps: bool,
    name: String,
}

fn parse_args() -> Result<Cli> {
    let mut cli = Cli {
        frames: 1,
        name: "dump-display".to_string(),
        ..Default::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => {
                cli.socket = Some(args.next().context("--socket needs PATH")?.into());
            }
            "--advertise" => {
                let v = args.next().context("--advertise needs FOURCC:MODIFIER")?;
                cli.advertise.push(parse_advertise(&v)?);
            }
            "--dump-dir" => {
                cli.dump_dir = Some(args.next().context("--dump-dir needs PATH")?.into());
            }
            "--frames" => {
                cli.frames = args
                    .next()
                    .context("--frames needs N")?
                    .parse()
                    .context("--frames N must be u32")?;
            }
            "--print-caps" => {
                cli.print_caps = true;
            }
            "--name" => {
                cli.name = args.next().context("--name needs STR")?;
            }
            "-h" | "--help" => {
                eprintln!(
                    "waywallen-dump-display — test consumer\n\
                     usage:\n  \
                     --print-caps                        emit PeerCaps JSON, exit\n  \
                     --socket PATH                       daemon UDS\n  \
                     --advertise FOURCC:MODIFIER         repeatable; FOURCC is hex u32 \
                                                         or 4-char ASCII, MODIFIER is hex u64\n  \
                     --dump-dir DIR                      where to write consumer-*.bin dumps\n  \
                     --frames N                          exit after N frames (default 1)\n  \
                     --name S                            display name in register_display\n"
                );
                std::process::exit(0);
            }
            other => bail!("unknown arg: {other}"),
        }
    }
    Ok(cli)
}

/// Parse `FOURCC:MODIFIER`. FOURCC is either `0x1234abcd` or a four-byte ASCII
/// like `'AB24'` (quotes optional); MODIFIER is hex `0x..` or decimal.
fn parse_advertise(s: &str) -> Result<(u32, u64)> {
    let (l, r) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("--advertise needs FOURCC:MODIFIER, got {s:?}"))?;
    let fourcc = parse_fourcc(l)?;
    let modifier = parse_u64_loose(r).context("modifier")?;
    Ok((fourcc, modifier))
}

fn parse_fourcc(s: &str) -> Result<u32> {
    let s = s.trim().trim_matches(['\'', '"']);
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return Ok(u32::from_str_radix(hex, 16).context("hex fourcc")?);
    }
    let bytes = s.as_bytes();
    if bytes.len() == 4 {
        return Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    }
    Ok(s.parse::<u32>().context("decimal fourcc")?)
}

fn parse_u64_loose(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return Ok(u64::from_str_radix(hex, 16)?);
    }
    Ok(s.parse::<u64>()?)
}

// ---------------------------------------------------------------------------
// PeerCaps JSON  (shared schema with image plugin's --print-caps)
// ---------------------------------------------------------------------------

/// Wire shape that mirrors `waywallen::negotiate::PeerCaps` minus the
/// blacklist (which is a runtime-only thing). Keep field names stable —
/// the image plugin's C++ `--print-caps` emits the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCapsJson {
    pub by_fourcc: BTreeMap<String, Vec<ModCapJson>>,
    pub device_uuid: [u8; 16],
    pub driver_uuid: [u8; 16],
    pub drm_render_major: u32,
    pub drm_render_minor: u32,
    pub sync: u32,
    pub color: u32,
    pub mem_hint: u32,
    pub extent_max_w: u32,
    pub extent_max_h: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModCapJson {
    pub modifier: u64,
    pub usage: u32,
    pub plane_count: u32,
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = parse_args()?;

    if cli.print_caps {
        let caps = vk_consumer::query_caps()?;
        println!("{}", serde_json::to_string_pretty(&caps)?);
        return Ok(());
    }

    let socket = cli
        .socket
        .as_ref()
        .ok_or_else(|| anyhow!("--socket required (or --print-caps)"))?;
    if cli.advertise.is_empty() {
        bail!("--advertise FOURCC:MODIFIER required at least once");
    }
    if let Some(d) = &cli.dump_dir {
        std::fs::create_dir_all(d).with_context(|| format!("mkdir {}", d.display()))?;
    }

    // Build the Vulkan context up-front so do_handshake can ship a
    // real device_uuid + DRM render-node id. The negotiate picker
    // gates "prefer non-LINEAR" on producer/consumer device equality
    // (`same_device`); reporting zeros forces the cross-vendor LINEAR
    // fallback, which wouldn't exercise the tiled-modifier path.
    let vk = vk_consumer::VkContext::new()?;

    let stream =
        UnixStream::connect(socket).with_context(|| format!("connect {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    do_handshake(&stream, &cli, &vk)?;
    consumer_loop(&stream, &cli, &vk)?;
    let _ = codec::send_request(&stream, &Request::Bye, &[]);
    Ok(())
}

fn do_handshake(stream: &UnixStream, cli: &Cli, vk: &vk_consumer::VkContext) -> Result<u64> {
    codec::send_request(
        stream,
        &Request::Hello {
            protocol: PROTOCOL_NAME.to_string(),
            client_name: cli.name.clone(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            client_protocol_version: PROTOCOL_VERSION,
        },
        &[],
    )
    .map_err(|e| anyhow!("send hello: {e}"))?;

    let (welcome, _) = codec::recv_event(stream).map_err(|e| anyhow!("recv welcome: {e}"))?;
    match welcome {
        Event::Welcome { server_version, .. } => {
            log::info!("welcome from {server_version}");
        }
        other => bail!("expected welcome, got opcode {}", other.opcode()),
    }

    codec::send_request(
        stream,
        &Request::RegisterDisplay {
            name: cli.name.clone(),
            instance_id: String::new(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
            drm_render_major: vk.drm_render_major,
            drm_render_minor: vk.drm_render_minor,
            properties: Vec::new(),
        },
        &[],
    )
    .map_err(|e| anyhow!("send register_display: {e}"))?;

    let (accepted, _) =
        codec::recv_event(stream).map_err(|e| anyhow!("recv display_accepted: {e}"))?;
    let display_id = match accepted {
        Event::DisplayAccepted { display_id } => display_id,
        Event::Error { code, message } => bail!("daemon Error: code={code} msg={message:?}"),
        other => bail!("expected display_accepted, got opcode {}", other.opcode()),
    };
    log::info!("display_accepted id={display_id}");

    // Send ConsumerCaps with exactly the (fourcc, modifier) pairs from
    // --advertise. Group by fourcc so the parallel arrays match the
    // wire shape negotiate::unflatten_caps expects.
    let mut by_fc: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
    for &(fc, m) in &cli.advertise {
        by_fc.entry(fc).or_default().push(m);
    }
    let mut fourccs = Vec::new();
    let mut mod_counts = Vec::new();
    let mut modifiers = Vec::new();
    let mut usages = Vec::new();
    let mut plane_counts = Vec::new();
    for (fc, mods) in &by_fc {
        fourccs.push(*fc);
        mod_counts.push(mods.len() as u32);
        for m in mods {
            modifiers.push(*m);
            // Conservative: SAMPLED + TRANSFER_DST is enough for a
            // consumer that just blits to a host buffer.
            usages.push(
                waywallen::negotiate::USAGE_SAMPLED | waywallen::negotiate::USAGE_TRANSFER_DST,
            );
            plane_counts.push(1);
        }
    }

    codec::send_request(
        stream,
        &Request::ConsumerCaps {
            fourccs,
            mod_counts,
            modifiers,
            usages,
            plane_counts,
            device_uuid: bytes16_to_u32x4(&vk.device_uuid),
            driver_uuid: bytes16_to_u32x4(&vk.driver_uuid),
            drm_render_major: vk.drm_render_major,
            drm_render_minor: vk.drm_render_minor,
            mem_hints: waywallen::negotiate::MEM_HINT_HOST_VISIBLE,
            sync_caps: waywallen::negotiate::SYNC_SYNCOBJ_BINARY
                | waywallen::negotiate::SYNC_SYNCOBJ_TIMELINE,
            color_caps: waywallen::negotiate::DEFAULT_COLOR,
            extent_max_w: 8192,
            extent_max_h: 8192,
        },
        &[],
    )
    .map_err(|e| anyhow!("send consumer_caps: {e}"))?;

    Ok(display_id)
}

/// Pack a 16-byte UUID into the 4×u32 wire form. The wire format is
/// little-endian within each u32 chunk, so we just `from_le_bytes`
/// each 4-byte slice. Mirrors what `negotiate::unflatten_caps` does
/// on the receiving side.
fn bytes16_to_u32x4(uuid: &[u8; 16]) -> Vec<u32> {
    (0..4)
        .map(|i| {
            let s = &uuid[i * 4..i * 4 + 4];
            u32::from_le_bytes([s[0], s[1], s[2], s[3]])
        })
        .collect()
}

#[derive(Default)]
struct BoundPool {
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    stride: Vec<u32>,
    plane_offset: Vec<u32>,
    size: Vec<u64>,
    fds: Vec<OwnedFd>,
}

fn consumer_loop(stream: &UnixStream, cli: &Cli, vk: &vk_consumer::VkContext) -> Result<()> {
    let drm = DrmDevice::open_first_render_node().context("open DRM render node")?;
    let mut pool: Option<BoundPool> = None;
    let mut frames_seen = 0u32;

    while frames_seen < cli.frames {
        let (evt, mut fds) = match codec::recv_event(stream) {
            Ok(x) => x,
            Err(e) => bail!("recv_event: {e}"),
        };
        match evt {
            Event::BindBuffers {
                buffer_generation: _,
                count,
                width,
                height,
                fourcc,
                modifier,
                planes_per_buffer,
                stride,
                plane_offset,
                size,
            } => {
                let expected = (count as usize) * (planes_per_buffer as usize);
                if fds.len() != expected {
                    bail!(
                        "BindBuffers: expected {expected} fds, got {} (count={count}, planes={planes_per_buffer})",
                        fds.len()
                    );
                }
                log::info!(
                    "bind: count={count} {width}x{height} fourcc=0x{fourcc:08x} \
                     modifier=0x{modifier:016x} planes={planes_per_buffer}"
                );
                pool = Some(BoundPool {
                    fourcc,
                    modifier,
                    width,
                    height,
                    stride,
                    plane_offset,
                    size,
                    fds: std::mem::take(&mut fds),
                });
            }
            Event::SetConfig { .. } => {
                // We don't do composition; nothing to apply.
            }
            Event::FrameReady {
                buffer_generation: _,
                buffer_index,
                seq,
            } => {
                if fds.len() != 2 {
                    bail!(
                        "FrameReady: expected 2 fds (acquire, release), got {}",
                        fds.len()
                    );
                }
                let release_fd = fds.remove(1);
                let acquire_fd = fds.remove(0);
                let p = pool
                    .as_ref()
                    .ok_or_else(|| anyhow!("FrameReady without prior BindBuffers"))?;
                let buf_idx = buffer_index as usize;
                if buf_idx >= p.fds.len() {
                    bail!(
                        "FrameReady buffer_index {buf_idx} out of pool {}",
                        p.fds.len()
                    );
                }

                if let Some(dump_dir) = &cli.dump_dir {
                    // For the Vulkan path the imported sync_fd is
                    // CONSUMED on success (SYNC_FD imports must be
                    // TEMPORARY per spec), and on failure the call
                    // closes its own dup. Either way `acquire_fd`'s
                    // OwnedFd Drop here is correct: it always still
                    // owns the original fd; Vulkan dups internally.
                    if let Err(e) = vk.import_and_dump(
                        &p.fds[buf_idx],
                        acquire_fd.as_raw_fd(),
                        p.fourcc,
                        p.modifier,
                        p.width,
                        p.height,
                        p.stride[buf_idx],
                        p.plane_offset[buf_idx],
                        p.size[buf_idx],
                        seq,
                        dump_dir,
                    ) {
                        // ALWAYS signal release_syncobj before bailing — the
                        // producer's release timeline will block forever
                        // otherwise and tear down with confusing diagnostics.
                        let _ = signal_release(&drm, &release_fd);
                        return Err(e.context("import_and_dump"));
                    }
                } else {
                    log::warn!("FrameReady seq={seq}: no --dump-dir, skipping import");
                }
                drop(acquire_fd); // close, regardless of dump path

                signal_release(&drm, &release_fd).context("signal release_syncobj")?;
                drop(release_fd);

                frames_seen += 1;
                log::info!("frame {seq} consumed (#{frames_seen}/{})", cli.frames);
            }
            Event::Error { code, message } => {
                bail!("daemon Error: code={code} msg={message:?}");
            }
            other => {
                log::debug!("ignoring event opcode {}", other.opcode());
            }
        }
    }
    Ok(())
}

/// Import a release_syncobj fd into the local DRM render node and signal it.
/// The producer's release timeline is gated on this binary syncobj; not
/// signaling will hang the producer indefinitely.
fn signal_release(drm: &DrmDevice, fd: &OwnedFd) -> anyhow::Result<()> {
    let handle = drm.fd_to_handle(fd)?;
    drm.signal(&handle)?;
    drop(handle);
    Ok(())
}
