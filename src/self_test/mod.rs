use std::path::PathBuf;

pub mod display_consumer;
pub mod orchestrator;
pub mod peer;
pub mod proto;
pub mod report;
pub mod spawn;
pub mod tests;
pub mod vk;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Orchestrator,
    Peer,
    Renderer,
    Display,
}

#[derive(Debug, Clone)]
pub struct TestArgs {
    pub role: Role,
    pub vk_uuid: Option<[u8; 16]>,
    pub socket: Option<PathBuf>,
    pub slot: u32,
    /// 1..=2 entries: `[orch_idx]` or `[orch_idx, child_idx]`. Empty
    /// means auto-pick (discrete > integrated > virtual > cpu) for
    /// both. Two distinct indices put the orchestrator and its
    /// children on different physical devices, exercising the
    /// cross-GPU dma-buf path.
    pub device_indices: Vec<usize>,
    pub skip_fanout: bool,
    // Display-child only: forwarded into `register_display`.
    pub display_name: Option<String>,
    pub instance_id: Option<String>,
    pub max_frames: Option<u64>,
}

impl TestArgs {
    fn parse(argv: &[String]) -> anyhow::Result<Self> {
        let mut role = Role::Orchestrator;
        let mut vk_uuid: Option<[u8; 16]> = None;
        let mut socket: Option<PathBuf> = None;
        let mut slot: u32 = 0;
        let mut device_indices: Vec<usize> = Vec::new();
        let mut skip_fanout = false;
        let mut display_name: Option<String> = None;
        let mut instance_id: Option<String> = None;
        let mut max_frames: Option<u64> = None;
        let mut it = argv.iter().skip(1).peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--test" => {}
                "--role" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--role requires a value"))?;
                    role = match v.as_str() {
                        "orchestrator" => Role::Orchestrator,
                        "peer" => Role::Peer,
                        "renderer" => Role::Renderer,
                        "display" => Role::Display,
                        other => anyhow::bail!("unknown role: {other}"),
                    };
                }
                "--vk-uuid" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--vk-uuid requires a value"))?;
                    vk_uuid = Some(parse_uuid_hex(v)?);
                }
                "--socket" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--socket requires a path"))?;
                    socket = Some(PathBuf::from(v));
                }
                "--slot" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--slot requires a value"))?;
                    slot = v.parse()?;
                }
                "--test-gpus" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--test-gpus requires a value"))?;
                    let mut idxs = Vec::new();
                    for part in v.split(',') {
                        let n: usize = part
                            .trim()
                            .parse()
                            .map_err(|e| anyhow::anyhow!("--test-gpus: bad index {part:?}: {e}"))?;
                        idxs.push(n);
                    }
                    if idxs.is_empty() {
                        anyhow::bail!("--test-gpus needs at least one index");
                    }
                    if idxs.len() > 2 {
                        anyhow::bail!("--test-gpus accepts at most 2 indices");
                    }
                    device_indices = idxs;
                }
                "--skip-fanout" => skip_fanout = true,
                "--display-name" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--display-name requires a value"))?;
                    display_name = Some(v.clone());
                }
                "--instance-id" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--instance-id requires a value"))?;
                    instance_id = Some(v.clone());
                }
                "--max-frames" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--max-frames requires a value"))?;
                    max_frames = Some(v.parse()?);
                }
                other => anyhow::bail!("unknown self-test arg: {other}"),
            }
        }
        Ok(TestArgs {
            role,
            vk_uuid,
            socket,
            slot,
            device_indices,
            skip_fanout,
            display_name,
            instance_id,
            max_frames,
        })
    }
}

pub fn run(argv: Vec<String>) -> anyhow::Result<()> {
    let args = TestArgs::parse(&argv)?;
    match args.role {
        Role::Orchestrator => orchestrator::run(args),
        Role::Peer => peer::run_peer(args),
        Role::Renderer => peer::run_renderer(args),
        Role::Display => peer::run_display(args),
    }
}

fn parse_uuid_hex(s: &str) -> anyhow::Result<[u8; 16]> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if cleaned.len() != 32 {
        anyhow::bail!("expected 32 hex chars, got {}", cleaned.len());
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

pub fn format_uuid_hex(b: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn uuid_roundtrip_strips_dashes() {
        let canonical = "f47ac10b-58cc-4372-a567-0e02b2c3d479";
        let bytes = parse_uuid_hex(canonical).unwrap();
        assert_eq!(format_uuid_hex(&bytes), "f47ac10b58cc4372a5670e02b2c3d479");
    }

    #[test]
    fn parse_args_role_orchestrator_default() {
        let argv = vec!["waywallen".into(), "--test".into()];
        let a = TestArgs::parse(&argv).unwrap();
        assert_eq!(a.role, Role::Orchestrator);
    }

    #[test]
    fn parse_args_role_peer_with_uuid() {
        let argv = vec![
            "waywallen".into(),
            "--test".into(),
            "--role".into(),
            "peer".into(),
            "--vk-uuid".into(),
            "f47ac10b58cc4372a5670e02b2c3d479".into(),
            "--socket".into(),
            "/tmp/x".into(),
        ];
        let a = TestArgs::parse(&argv).unwrap();
        assert_eq!(a.role, Role::Peer);
        assert!(a.vk_uuid.is_some());
        assert_eq!(a.socket.as_deref(), Some(std::path::Path::new("/tmp/x")));
    }
}
