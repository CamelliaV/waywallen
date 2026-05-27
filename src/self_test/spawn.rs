use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{anyhow, Context, Result};

use super::format_uuid_hex;

pub fn make_socket_path(tag: &str) -> Result<PathBuf> {
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set"))?;
    let dir = PathBuf::from(runtime).join("waywallen");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).context("create $XDG_RUNTIME_DIR/waywallen")?;
    }
    Ok(dir.join(format!("test-{}-{tag}.sock", std::process::id())))
}

#[derive(Debug, Clone)]
pub struct ChildSpec {
    pub role: &'static str,
    pub socket: PathBuf,
    pub vk_uuid: [u8; 16],
    pub slot: u32,
    /// Display-only: forwarded as `--display-name`. Ignored for other roles.
    pub display_name: Option<String>,
    /// Display-only: forwarded as `--instance-id`. Empty → not passed.
    pub instance_id: Option<String>,
    /// Display-only: forwarded as `--max-frames` (default 60 in child).
    pub max_frames: Option<u64>,
    /// Capture child stdout instead of inheriting. Display children
    /// emit a single JSON status line on exit; orchestrator parses it.
    pub capture_stdout: bool,
}

pub fn spawn(spec: &ChildSpec) -> Result<Child> {
    let exe = std::env::current_exe().context("current_exe")?;
    let mut cmd = Command::new(exe);
    cmd.env_clear();
    if let Ok(v) = std::env::var("XDG_RUNTIME_DIR") {
        cmd.env("XDG_RUNTIME_DIR", v);
    }
    if let Ok(v) = std::env::var("RUST_LOG") {
        cmd.env("RUST_LOG", v);
    }
    if let Ok(v) = std::env::var("PATH") {
        // Some libcs stall on AT_PLATFORM init when PATH is empty; keep
        // it even though the dynamic linker doesn't strictly need it.
        cmd.env("PATH", v);
    }
    cmd.arg("--test")
        .arg("--role")
        .arg(spec.role)
        .arg("--socket")
        .arg(&spec.socket)
        .arg("--vk-uuid")
        .arg(format_uuid_hex(&spec.vk_uuid))
        .arg("--slot")
        .arg(spec.slot.to_string());
    if let Some(name) = &spec.display_name {
        cmd.arg("--display-name").arg(name);
    }
    if let Some(id) = &spec.instance_id {
        if !id.is_empty() {
            cmd.arg("--instance-id").arg(id);
        }
    }
    if let Some(n) = spec.max_frames {
        cmd.arg("--max-frames").arg(n.to_string());
    }
    cmd.stdin(Stdio::null());
    if spec.capture_stdout {
        cmd.stdout(Stdio::piped());
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn child role={}", spec.role))?;
    Ok(child)
}

pub fn bind_listener(path: &Path) -> Result<(std::os::unix::net::UnixListener, SocketCleanup)> {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    let listener = std::os::unix::net::UnixListener::bind(path)
        .with_context(|| format!("bind {}", path.display()))?;
    Ok((
        listener,
        SocketCleanup {
            path: path.to_path_buf(),
        },
    ))
}

pub struct SocketCleanup {
    path: PathBuf,
}
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
