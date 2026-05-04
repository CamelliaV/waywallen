#![allow(dead_code)]
//! Shared helpers for integration tests.
//!
//! Each file under `tests/*.rs` is compiled as its own crate, so shared
//! code must be pulled in with `#[path = "common/mod.rs"] mod common;`.
//! `#![allow(dead_code)]` silences warnings in files that only use a
//! subset of the helpers.

use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Allocate a unique tempfile path for a Unix-domain socket. The pid and
/// a nanosecond timestamp keep parallel tests from colliding.
pub fn tmp_sock(tag: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("waywallen-{tag}-{}-{ts}.sock", std::process::id()))
}

/// RAII guard that unlinks the socket file on drop. Safe to hold for the
/// full test body — the listener fd stays valid after unlink.
pub struct SockCleanup(pub PathBuf);
impl Drop for SockCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// RAII wrapper around a `Child` that SIGKILLs + waits on drop so a
/// failing test never leaks a renderer process.
pub struct ChildGuard(pub Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `UnixListener::accept` with a bounded wall-clock timeout. Returns
/// `None` on timeout (caller decides whether to panic or skip).
pub fn accept_with_timeout(
    listener: &UnixListener,
    timeout: Duration,
) -> Option<std::io::Result<(UnixStream, SocketAddr)>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let l_clone = listener.try_clone().expect("clone listener");
    std::thread::spawn(move || {
        let _ = tx.send(l_clone.accept());
    });
    rx.recv_timeout(timeout).ok()
}

/// Poll `path.exists()` until true or `timeout` elapses. Used to wait
/// for the display endpoint to finish `UnixListener::bind` before
/// clients attempt to connect.
pub async fn wait_for_sock_bind(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Cheap DRM render-node presence check. Vulkan-only tests should
/// `return` with a `skip:` eprintln when this returns `false`.
pub fn have_vulkan_device() -> bool {
    Path::new("/dev/dri").exists()
}

/// Resolve the external C++ host binary from `WAYWALLEN_RENDERER_BIN`.
/// Tests that need the C++ host should early-return with a skip line
/// when this yields `None`.
pub fn cpp_renderer_bin_from_env() -> Option<PathBuf> {
    std::env::var_os("WAYWALLEN_RENDERER_BIN").map(PathBuf::from)
}

/// Resolve the `waywallen-dump-display` test binary. Searches:
///   1. `$WAYWALLEN_DUMP_DISPLAY_BIN` (test override),
///   2. cargo's per-crate `CARGO_BIN_EXE_*`-style fallback under
///      `target/{debug,release}/` relative to `CARGO_MANIFEST_DIR`,
///   3. `$CARGO_TARGET_DIR/{debug,release}/`.
///
/// Returns `None` when the bin isn't built — caller should
/// `eprintln!("skip:")` and return.
pub fn dump_display_bin_from_env() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WAYWALLEN_DUMP_DISPLAY_BIN").map(PathBuf::from) {
        if p.exists() {
            return Some(p);
        }
    }
    let bin_name = "waywallen-dump-display";
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Workspace target dir lives at <manifest>/target by default.
    let candidates = [
        manifest.join("target/debug").join(bin_name),
        manifest.join("target/release").join(bin_name),
    ];
    for c in &candidates {
        if c.exists() {
            return Some(c.clone());
        }
    }
    if let Some(td) = std::env::var_os("CARGO_TARGET_DIR") {
        for sub in &["debug", "release"] {
            let p = PathBuf::from(&td).join(sub).join(bin_name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// Snapshot of what one peer (renderer or consumer) reports via
/// `--print-caps`. Mirrors the JSON schema both sides emit; mirrors
/// `negotiate::PeerCaps` in spirit but stays JSON-friendly with
/// hex-string fourcc keys (the producer's printf can't easily emit
/// `BTreeMap<u32, _>` keys).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PeerCapsSnapshot {
    pub by_fourcc: std::collections::BTreeMap<String, Vec<ModCapEntry>>,
    #[serde(default)]
    pub device_uuid: [u8; 16],
    #[serde(default)]
    pub driver_uuid: [u8; 16],
    #[serde(default)]
    pub drm_render_major: u32,
    #[serde(default)]
    pub drm_render_minor: u32,
    pub sync: u32,
    pub color: u32,
    pub mem_hint: u32,
    pub extent_max_w: u32,
    pub extent_max_h: u32,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModCapEntry {
    pub modifier: u64,
    pub usage: u32,
    pub plane_count: u32,
}

impl PeerCapsSnapshot {
    /// Iterate `(fourcc_u32, modifier)` pairs with the hex string
    /// keys decoded back to integers. Skips entries whose key isn't
    /// parseable as `0x[0-9a-fA-F]{1,8}`.
    pub fn pairs(&self) -> Vec<(u32, u64)> {
        let mut out = Vec::new();
        for (k, mods) in &self.by_fourcc {
            let Some(stripped) = k.strip_prefix("0x").or_else(|| k.strip_prefix("0X")) else {
                continue;
            };
            let Ok(fc) = u32::from_str_radix(stripped, 16) else {
                continue;
            };
            for m in mods {
                out.push((fc, m.modifier));
            }
        }
        out
    }
}

/// Run `bin --print-caps`, capture stdout, parse as `PeerCapsSnapshot`.
/// Returns `Err` (not a skip) if the binary exits non-zero or stdout
/// isn't valid JSON — both are real bugs we want to catch.
pub fn print_caps(bin: &Path) -> std::io::Result<PeerCapsSnapshot> {
    let out = std::process::Command::new(bin)
        .arg("--print-caps")
        .stderr(std::process::Stdio::inherit())
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "{} --print-caps exited {:?}",
            bin.display(),
            out.status.code()
        )));
    }
    let stdout = String::from_utf8(out.stdout)
        .map_err(|e| std::io::Error::other(format!("non-utf8 stdout: {e}")))?;
    serde_json::from_str(&stdout).map_err(|e| {
        std::io::Error::other(format!("parse caps json: {e}\n--- stdout ---\n{stdout}"))
    })
}

/// Compute the intersection of two cap snapshots: every `(fourcc,
/// modifier)` pair present on **both** sides. Sorted ascending for
/// deterministic test iteration.
pub fn intersect_caps(a: &PeerCapsSnapshot, b: &PeerCapsSnapshot) -> Vec<(u32, u64)> {
    use std::collections::BTreeSet;
    let aset: BTreeSet<(u32, u64)> = a.pairs().into_iter().collect();
    let bset: BTreeSet<(u32, u64)> = b.pairs().into_iter().collect();
    aset.intersection(&bset).copied().collect()
}

/// Compare a producer dump with the matching consumer dump. Both
/// files must hold tightly-packed RGBA8 (`width * height * 4` bytes)
/// per the schema set by image plugin's `maybe_dump_producer_frame`
/// and dump_display's `import_and_dump`. Returns `Ok(())` on byte
/// equality. On mismatch returns the byte index of the first
/// disagreement plus the values (useful in panic messages).
pub fn compare_rgba8_dumps(producer: &Path, consumer: &Path) -> std::io::Result<()> {
    let a = std::fs::read(producer)?;
    let b = std::fs::read(consumer)?;
    if a.len() != b.len() {
        return Err(std::io::Error::other(format!(
            "size mismatch: producer={} consumer={} ({} vs {} bytes)",
            producer.display(),
            consumer.display(),
            a.len(),
            b.len(),
        )));
    }
    if let Some(idx) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
        return Err(std::io::Error::other(format!(
            "first byte mismatch at offset {idx}: producer={:#04x} consumer={:#04x}",
            a[idx], b[idx]
        )));
    }
    Ok(())
}
