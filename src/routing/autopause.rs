//! Per-display autopause: decode the consumer's `window_state` bitmask
//! into an "autopause requested?" boolean and apply the resume debounce
//! that smooths bursty fullscreen toggles.
//!
//! The router owns the per-display [`State`] machine; this module is
//! pure logic + bit constants. `decide` mirrors the modes of the old
//! KDE wallpaper plugin's `Common.PauseMode` (now `AutopauseMode`),
//! and the resume debounce mirrors the old `playTimer` — pause is
//! immediate, but resuming is delayed by `resume_ms` so a brief flag
//! drop doesn't flap the renderer.

use crate::settings::AutopauseMode;

/// Some mapped (non-minimized) window covers this display.
pub const FLAG_NON_MINIMIZED: u32 = 1 << 0;
/// Some window on this display has keyboard focus.
pub const FLAG_ACTIVE: u32 = 1 << 1;
/// Some window is H+V maximized (and NOT fullscreen).
pub const FLAG_MAXIMIZED: u32 = 1 << 2;
/// Some window is fullscreen.
pub const FLAG_FULLSCREEN: u32 = 1 << 3;

/// Bits the daemon understands. Higher bits are reserved and ignored.
pub const FLAGS_KNOWN: u32 = FLAG_NON_MINIMIZED | FLAG_ACTIVE | FLAG_MAXIMIZED | FLAG_FULLSCREEN;

/// Pure mapping: (mode, flags) → "autopause this display?".
pub fn decide(mode: AutopauseMode, flags: u32) -> bool {
    let has = |b: u32| flags & b != 0;
    match mode {
        AutopauseMode::Never => false,
        AutopauseMode::Any => has(FLAG_NON_MINIMIZED),
        AutopauseMode::Focus => has(FLAG_ACTIVE),
        AutopauseMode::Max => has(FLAG_MAXIMIZED) || has(FLAG_FULLSCREEN),
        AutopauseMode::FocusOrMax => {
            has(FLAG_ACTIVE) || has(FLAG_MAXIMIZED) || has(FLAG_FULLSCREEN)
        }
        AutopauseMode::FullScreen => has(FLAG_FULLSCREEN),
    }
}

/// Per-display autopause state held by the router.
#[derive(Debug, Default)]
pub struct State {
    /// Most recent flags the consumer reported.
    pub last_flags: u32,
    /// `decide(mode, last_flags)` — instantaneous raw signal.
    pub raw_want_pause: bool,
    /// Effective signal consumed by `reconcile_lifecycle`. Equals
    /// `raw_want_pause` immediately on pause transitions; on
    /// pause→play it stays `true` until the resume timer fires.
    pub requested: bool,
    /// Bumped on every transition. A pending resume task carries the
    /// generation snapshot taken when it was spawned, and is a no-op
    /// on fire if the counter has advanced (i.e. a newer transition
    /// invalidated it).
    pub gen: u64,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_is_never() {
        for flags in [
            0,
            FLAG_NON_MINIMIZED,
            FLAG_ACTIVE,
            FLAG_FULLSCREEN,
            0xFFFFFFFF,
        ] {
            assert!(!decide(AutopauseMode::Never, flags));
        }
    }

    #[test]
    fn any_fires_on_non_minimized() {
        assert!(!decide(AutopauseMode::Any, 0));
        assert!(decide(AutopauseMode::Any, FLAG_NON_MINIMIZED));
        // Bit 0 should be the dominant signal here — fullscreen alone
        // without NON_MINIMIZED is malformed but documents the rule.
        assert!(!decide(AutopauseMode::Any, FLAG_FULLSCREEN));
    }

    #[test]
    fn focus_fires_only_on_active() {
        assert!(!decide(AutopauseMode::Focus, FLAG_MAXIMIZED));
        assert!(decide(AutopauseMode::Focus, FLAG_ACTIVE));
    }

    #[test]
    fn max_covers_fullscreen() {
        assert!(decide(AutopauseMode::Max, FLAG_MAXIMIZED));
        assert!(decide(AutopauseMode::Max, FLAG_FULLSCREEN));
        assert!(!decide(AutopauseMode::Max, FLAG_ACTIVE));
        assert!(!decide(AutopauseMode::Max, FLAG_NON_MINIMIZED));
    }

    #[test]
    fn focus_or_max_is_union() {
        assert!(decide(AutopauseMode::FocusOrMax, FLAG_ACTIVE));
        assert!(decide(AutopauseMode::FocusOrMax, FLAG_MAXIMIZED));
        assert!(decide(AutopauseMode::FocusOrMax, FLAG_FULLSCREEN));
        assert!(!decide(AutopauseMode::FocusOrMax, FLAG_NON_MINIMIZED));
    }

    #[test]
    fn fullscreen_is_strict() {
        assert!(!decide(AutopauseMode::FullScreen, FLAG_MAXIMIZED));
        assert!(decide(AutopauseMode::FullScreen, FLAG_FULLSCREEN));
    }

    #[test]
    fn unknown_bits_ignored() {
        let stray = 1 << 30;
        assert!(!decide(AutopauseMode::Any, stray));
        assert!(decide(AutopauseMode::Any, stray | FLAG_NON_MINIMIZED));
    }
}
