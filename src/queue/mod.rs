//! Queue subsystem: the daemon's single in-memory playback queue.
//!
//! User-facing "playlists" (named saved bundles) are a separate future
//! concept; loading one will mean populating *this* queue from the
//! saved filter/items.
//!
//! The queue plays from `settings.global.wallpaper_filter` (proto rules
//! + logics). No item snapshot lives here — `control::step` queries
//! the DB on demand. The only state owned by this module is mode +
//! cursor + shuffle round.

pub mod rotator;
pub mod state;

pub use rotator::{RotationConfig, RotationHandle};
pub use state::{Mode, QueueState};

/// Strip `library_root` from `resource` and return the path remainder.
/// Both ends are normalized for trailing slashes. Returns `None` when
/// `resource` does not live under `library_root`.
pub fn relative_under_root(library_root: &str, resource: &str) -> Option<String> {
    let root = library_root.trim_end_matches('/');
    let rest = resource.strip_prefix(root)?;
    Some(rest.trim_start_matches('/').to_owned())
}
