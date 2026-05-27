use std::io::Write;

use serde::Serialize;

use super::vk::instance::DeviceMeta;

#[derive(Default, Serialize)]
pub struct Report {
    pub build: String,
    pub date_utc: String,
    pub fatal: Option<String>,
    pub devices: Vec<DeviceMeta>,
    pub picked_device_index: Option<usize>,
    /// Set only when the children run on a different physical device
    /// than the orchestrator (cross-GPU dma-buf path).
    pub child_device_index: Option<usize>,
    pub modifier_matrix: Option<ModifierMatrix>,
    pub render_loop: Option<RenderLoop>,
    pub fanout: Option<Fanout>,
}

#[derive(Default, Serialize)]
pub struct ModifierMatrix {
    pub modifiers: Vec<ModifierResult>,
}

#[derive(Serialize)]
pub struct ModifierResult {
    pub fourcc: u32,
    pub modifier: u64,
    pub modifier_name: String,
    pub producer: ProbeOutcome,
    pub consumer: ProbeOutcome,
}

#[derive(Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    Ok,
    Fail { vk_result: i32, message: String },
}

#[derive(Default, Serialize)]
pub struct RenderLoop {
    pub frames: u32,
    pub ok: u32,
    pub color_mismatch: u32,
    pub acquire_timeout: u32,
    pub modifier_used: u64,
    pub modifier_name: String,
}

#[derive(Default, Serialize)]
pub struct Fanout {
    pub frames: u32,
    pub ok: u32,
    pub display_kill_at: Option<u32>,
    pub kill_recovered_ms: Option<u128>,
    pub refcount_leaks: u32,
}

impl Report {
    pub fn new() -> Self {
        Self {
            build: env!("CARGO_PKG_VERSION").to_string(),
            date_utc: unix_now(),
            ..Default::default()
        }
    }

    pub fn fatal(&mut self, msg: impl Into<String>) {
        self.fatal = Some(msg.into());
    }

    pub fn note_devices(&mut self, devs: &[DeviceMeta]) {
        self.devices = devs.to_vec();
    }

    pub fn note_picked_device(&mut self, dev: &DeviceMeta) {
        self.picked_device_index = self.devices.iter().position(|d| d.uuid == dev.uuid);
    }

    pub fn note_child_device(&mut self, dev: &DeviceMeta) {
        self.child_device_index = self.devices.iter().position(|d| d.uuid == dev.uuid);
    }

    pub fn emit(&self) {
        let mut stdout = std::io::stdout().lock();
        let _ = self.write_summary(&mut stdout);
        let path = format!("./waywallen-test-{}.md", std::process::id());
        let mut buf = Vec::<u8>::new();
        let _ = self.write_markdown(&mut buf);
        match std::fs::write(&path, &buf) {
            Ok(()) => {
                let _ = writeln!(stdout, "report:           {path}");
            }
            Err(e) => {
                let _ = writeln!(stdout, "(could not write {path}: {e})");
            }
        }
    }

    fn write_summary(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            w,
            "waywallen --test  (build {}, {})",
            self.build, self.date_utc
        )?;
        if let Some(msg) = &self.fatal {
            writeln!(w, "FATAL:            {msg}")?;
            writeln!(w, "VERDICT:          fail")?;
            return Ok(());
        }
        writeln!(w, "devices:")?;
        for (i, d) in self.devices.iter().enumerate() {
            let mark = self.device_role_mark(i);
            writeln!(
                w,
                "  {mark} [{i}] {}  uuid={}",
                d.name,
                super::format_uuid_hex(&d.uuid),
            )?;
        }
        if let Some(a) = &self.modifier_matrix {
            let ok = a
                .modifiers
                .iter()
                .filter(|m| {
                    matches!(m.producer, ProbeOutcome::Ok) && matches!(m.consumer, ProbeOutcome::Ok)
                })
                .count();
            writeln!(
                w,
                "modifier_matrix:  {} modifier(s) tested, {} ok",
                a.modifiers.len(),
                ok,
            )?;
        }
        if let Some(b) = &self.render_loop {
            writeln!(
                w,
                "render_loop:      {} frames, modifier={}; ok={} mismatch={} timeout={}",
                b.frames, b.modifier_name, b.ok, b.color_mismatch, b.acquire_timeout,
            )?;
        }
        if let Some(c) = &self.fanout {
            writeln!(
                w,
                "fanout:           {} frames; ok={} refcount_leaks={}",
                c.frames, c.ok, c.refcount_leaks,
            )?;
        }
        writeln!(w, "VERDICT:          {}", self.verdict())?;
        Ok(())
    }

    fn write_markdown(&self, w: &mut impl Write) -> std::io::Result<()> {
        writeln!(w, "# waywallen --test")?;
        writeln!(w)?;
        writeln!(
            w,
            "build {} · {} · verdict **{}**",
            self.build,
            self.date_utc,
            self.verdict()
        )?;
        if let Some(msg) = &self.fatal {
            writeln!(w)?;
            writeln!(w, "**FATAL:** {msg}")?;
            return Ok(());
        }

        writeln!(w)?;
        writeln!(w, "## vulkan devices")?;
        writeln!(w)?;
        writeln!(w, "| role | # | name | uuid | type |")?;
        writeln!(w, "|:----:|--:|------|------|------|")?;
        for (i, d) in self.devices.iter().enumerate() {
            let role = match (
                Some(i) == self.picked_device_index,
                Some(i) == self.child_device_index,
            ) {
                (true, true) => "orch+child",
                (true, false) => "orch",
                (false, true) => "child",
                (false, false) => "",
            };
            writeln!(
                w,
                "| {role} | {i} | {} | `{}` | {:?} |",
                d.name,
                super::format_uuid_hex(&d.uuid),
                d.kind,
            )?;
        }

        if let Some(a) = &self.modifier_matrix {
            writeln!(w)?;
            writeln!(w, "## modifier matrix")?;
            writeln!(w)?;
            writeln!(w, "| modifier | name | producer | consumer |")?;
            writeln!(w, "|----------|------|:--------:|:--------:|")?;
            for m in &a.modifiers {
                writeln!(
                    w,
                    "| `{:#018x}` | {} | {} | {} |",
                    m.modifier,
                    m.modifier_name,
                    fmt_outcome(&m.producer),
                    fmt_outcome(&m.consumer),
                )?;
            }
        }

        if let Some(b) = &self.render_loop {
            writeln!(w)?;
            writeln!(w, "## render loop")?;
            writeln!(w)?;
            writeln!(w, "| frames | ok | mismatch | timeout | modifier |")?;
            writeln!(w, "|------:|---:|---------:|--------:|----------|")?;
            writeln!(
                w,
                "| {} | {} | {} | {} | `{:#x}` ({}) |",
                b.frames,
                b.ok,
                b.color_mismatch,
                b.acquire_timeout,
                b.modifier_used,
                b.modifier_name,
            )?;
        }

        if let Some(c) = &self.fanout {
            writeln!(w)?;
            writeln!(w, "## fanout")?;
            writeln!(w)?;
            let kill_col = match (c.display_kill_at, c.kill_recovered_ms) {
                (Some(at), Some(ms)) => format!("@{at} ({ms}ms)"),
                (Some(at), None) => format!("@{at} (?)"),
                (None, _) => "-".into(),
            };
            writeln!(w, "| frames | ok | refcount leaks | display kill |")?;
            writeln!(w, "|------:|---:|---------------:|:-------------|")?;
            writeln!(
                w,
                "| {} | {} | {} | {} |",
                c.frames, c.ok, c.refcount_leaks, kill_col,
            )?;
        }

        Ok(())
    }

    fn verdict(&self) -> &'static str {
        if self.fatal.is_some() {
            return "fail";
        }
        let any_fail_matrix = self
            .modifier_matrix
            .as_ref()
            .map(|a| {
                a.modifiers.iter().any(|m| {
                    matches!(m.producer, ProbeOutcome::Fail { .. })
                        || matches!(m.consumer, ProbeOutcome::Fail { .. })
                })
            })
            .unwrap_or(false);
        let render_fail = self
            .render_loop
            .as_ref()
            .map(|b| b.color_mismatch > 0 || b.acquire_timeout > 0)
            .unwrap_or(false);
        let fanout_fail = self
            .fanout
            .as_ref()
            .map(|c| c.refcount_leaks > 0)
            .unwrap_or(false);
        if any_fail_matrix || render_fail || fanout_fail {
            "pass-with-warnings"
        } else {
            "pass"
        }
    }
}

impl Report {
    fn device_role_mark(&self, i: usize) -> &'static str {
        match (
            Some(i) == self.picked_device_index,
            Some(i) == self.child_device_index,
        ) {
            (true, true) => "*+",
            (true, false) => "* ",
            (false, true) => "+ ",
            (false, false) => "  ",
        }
    }
}

fn fmt_outcome(o: &ProbeOutcome) -> String {
    match o {
        ProbeOutcome::Ok => "ok".into(),
        ProbeOutcome::Fail { vk_result, message } => {
            format!("FAIL({vk_result}, {message})")
        }
    }
}

fn unix_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix={secs}")
}
