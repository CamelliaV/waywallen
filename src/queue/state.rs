//! Queue cursor state.
//!
//! Plays from a filter (proto rules + logics, sourced from
//! `settings.global.wallpaper_filter`) — no in-memory item list. The
//! step routines that need DB access live in `crate::control::step`.
//! This module is pure data + RNG + shuffle round materialization
//! helpers.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Sequential,
    Shuffle,
    Random,
}

impl Mode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Sequential => "sequential",
            Mode::Shuffle => "shuffle",
            Mode::Random => "random",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "sequential" => Some(Mode::Sequential),
            "shuffle" => Some(Mode::Shuffle),
            "random" => Some(Mode::Random),
            _ => None,
        }
    }
}

pub type ShuffleSeed = u64;

#[derive(Debug, Clone, Default)]
pub struct QueueState {
    pub mode: Mode,
    /// Last-applied entry id (UI display).
    pub current: Option<String>,
    /// DB id of the last-applied item. Anchor for sequential/random
    /// stepping; survives daemon restarts via settings persistence
    /// (Phase 5 wires this).
    pub last_db_id: Option<i64>,
    /// Permutation of DB ids matching the active filter at the time
    /// the round was built. Empty = no round; rebuilt lazily by the
    /// shuffle step path.
    pub shuffle_round: Vec<i64>,
    pub shuffle_pos: usize,
    pub shuffle_seed: ShuffleSeed,
    /// xorshift64 state. 0 = uninitialized; first use seeds from
    /// `shuffle_seed` or system time.
    pub rng: u64,
}

impl QueueState {
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        self.shuffle_round.clear();
        self.shuffle_pos = 0;
    }

    /// Reseed if the user explicitly requested a fresh shuffle.
    pub fn reset_shuffle_round(&mut self) {
        self.shuffle_round.clear();
        self.shuffle_pos = 0;
    }

    /// Build a fresh permutation of `candidates` into `shuffle_round`.
    /// If `avoid` is `Some`, the slot at `target_pos` is guaranteed
    /// not to equal it — used so wrap-forward (target=0) and
    /// wrap-backward (target=len-1) never replay the just-played id.
    pub fn build_shuffle_round(
        &mut self,
        candidates: Vec<i64>,
        avoid: Option<i64>,
        target_pos: usize,
    ) {
        self.shuffle_round = candidates;
        let n = self.shuffle_round.len();
        for i in (1..n).rev() {
            let j = self.rng_range((i + 1) as u32) as usize;
            self.shuffle_round.swap(i, j);
        }
        if let Some(av) = avoid {
            if n > 1 && self.shuffle_round.get(target_pos).copied() == Some(av) {
                let raw = self.rng_range((n - 1) as u32) as usize;
                let alt = if raw >= target_pos { raw + 1 } else { raw };
                self.shuffle_round.swap(target_pos, alt);
            }
        }
    }

    pub fn rng_range(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        (self.rng_next() % n as u64) as u32
    }

    fn rng_next(&mut self) -> u64 {
        if self.rng == 0 {
            let seed = if self.shuffle_seed != 0 {
                self.shuffle_seed
            } else {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0)
            };
            self.rng = if seed == 0 {
                0xdead_beef_cafe_babe
            } else {
                seed
            };
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_str_roundtrip() {
        for m in [Mode::Sequential, Mode::Shuffle, Mode::Random] {
            assert_eq!(Mode::from_str(m.as_str()), Some(m));
        }
        assert_eq!(Mode::from_str("nonsense"), None);
    }

    #[test]
    fn set_mode_resets_shuffle_round() {
        let mut s = QueueState::default();
        s.shuffle_round = vec![1, 2, 3];
        s.shuffle_pos = 2;
        s.set_mode(Mode::Shuffle);
        assert!(s.shuffle_round.is_empty());
        assert_eq!(s.shuffle_pos, 0);
    }

    #[test]
    fn shuffle_round_avoids_target_slot() {
        let mut s = QueueState::default();
        s.shuffle_seed = 42;
        // Build a 5-id round avoiding 3 at slot 0.
        s.build_shuffle_round(vec![1, 2, 3, 4, 5], Some(3), 0);
        assert_ne!(s.shuffle_round[0], 3);
        assert_eq!(s.shuffle_round.len(), 5);
        let mut sorted = s.shuffle_round.clone();
        sorted.sort();
        assert_eq!(sorted, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn rng_seeds_from_shuffle_seed_when_set() {
        let mut s = QueueState::default();
        s.shuffle_seed = 12345;
        let r1 = s.rng_range(1000);
        let mut s2 = QueueState::default();
        s2.shuffle_seed = 12345;
        let r2 = s2.rng_range(1000);
        assert_eq!(r1, r2, "same seed → same first draw");
    }
}
