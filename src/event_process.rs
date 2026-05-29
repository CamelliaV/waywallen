//! Central dispatcher for the daemon's `GlobalEvent` bus.
//!
//! Other modules `publish` into [`crate::events::EventBus`]; this task
//! holds the single in-process subscriber that reacts to phase markers
//! and spawns dependent work (e.g. wallpaper recall once
//! `SourcesReady` fires). New cross-cutting reactions should grow here
//! instead of new ad-hoc subscribe loops scattered across the daemon.
//!
//! WS clients still subscribe per-connection (`ws_server::dispatch`)
//! — that path translates events to protobuf for the UI and is a
//! distinct concern from this dispatcher.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::events::GlobalEvent;
use crate::routing;
use crate::scheduler;
use crate::tasks;
use crate::AppState;

/// Spawn the dispatcher. `restore_last` mirrors `cli.restore_last` —
/// when false the wallpaper-recall watcher is never started even
/// after `SourcesReady` fires.
pub fn spawn(state: Arc<AppState>, restore_last: bool) {
    let tasks_h = state.tasks.clone();
    tasks_h.spawn_async(
        tasks::TaskKind::Service,
        "service/event-process",
        async move {
            // Subscribe BEFORE re-reading the latches so an event that
            // fired between AppState construction and the first poll
            // of this task is still caught (via the latch).
            let mut bus = state.events.subscribe();
            let mut recall_started = !restore_last;

            if !recall_started && state.events.is_sources_ready() {
                spawn_wallpaper_recall(state.clone());
                recall_started = true;
            }

            loop {
                match bus.recv().await {
                    Ok(GlobalEvent::SourcesReady) => {
                        if !recall_started {
                            spawn_wallpaper_recall(state.clone());
                            recall_started = true;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if !recall_started && state.events.is_sources_ready() {
                            spawn_wallpaper_recall(state.clone());
                            recall_started = true;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Ok(());
                    }
                }
            }
        },
    );
}

/// Long-lived watcher: re-apply each display's persisted wallpaper as
/// it becomes visible. Spawned by the dispatcher when `SourcesReady`
/// fires so `source_snapshot` is guaranteed populated when the first
/// apply runs. Single path covers startup + hot-plug.
///
/// Coalescing rule: arrivals are grouped by `wp_id`. Each group fires
/// one `apply_wallpaper_to_displays` call after a SETTLE window from
/// the first arrival in the group, so two displays pointing at the
/// same wallpaper share a single renderer process (the apply path's
/// `find_reusable` only works when same-wp_id calls don't race in
/// parallel).
fn spawn_wallpaper_recall(state: Arc<AppState>) {
    let tasks_h = state.tasks.clone();
    tasks_h.spawn_async(
        tasks::TaskKind::Service,
        "service/wallpaper-recall",
        async move {
            // Settle window: how long to wait after the first display
            // for the group joins before firing the apply.
            const SETTLE: Duration = Duration::from_secs(2);
            // Far-future placeholder when nothing is pending, so the
            // select loop has a real deadline to wait on without an
            // extra `Option<Sleep>` arm.
            const IDLE_PARK: Duration = Duration::from_secs(3600);

            let mut seen: HashSet<scheduler::DisplayId> = HashSet::new();
            // wp_id -> (deadline, accumulated display ids)
            let mut pending: HashMap<String, (tokio::time::Instant, Vec<scheduler::DisplayId>)> =
                HashMap::new();
            let mut events_rx = state.router.subscribe_events();

            // Initial sweep of already-registered displays.
            for snap in state.router.snapshot_displays().await {
                if seen.insert(snap.id) {
                    record(&state, &mut pending, snap, SETTLE, true);
                }
            }

            loop {
                let next_deadline = pending
                    .values()
                    .map(|(d, _)| *d)
                    .min()
                    .unwrap_or_else(|| tokio::time::Instant::now() + IDLE_PARK);
                let sleep = tokio::time::sleep_until(next_deadline);
                tokio::pin!(sleep);

                tokio::select! {
                    ev = events_rx.recv() => {
                        let snaps: Vec<routing::DisplaySnapshot> = match ev {
                            Ok(routing::RouterEvent::DisplayUpsert(s)) => vec![s],
                            Ok(routing::RouterEvent::DisplaysReplace(list)) => list,
                            Ok(_) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                state.router.snapshot_displays().await
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                return Ok(());
                            }
                        };
                        for snap in snaps {
                            if seen.insert(snap.id) {
                                record(&state, &mut pending, snap, SETTLE, false);
                            }
                        }
                    }
                    _ = &mut sleep => {
                        let now = tokio::time::Instant::now();
                        let due: Vec<String> = pending
                            .iter()
                            .filter_map(|(k, (d, _))| (*d <= now).then(|| k.clone()))
                            .collect();
                        for wp_id in due {
                            if let Some((_, ids)) = pending.remove(&wp_id) {
                                let state2 = state.clone();
                                tokio::spawn(async move {
                                    log::info!(
                                        "wallpaper recall: applying {wp_id} to {} display(s)",
                                        ids.len()
                                    );
                                    if let Err(e) = crate::control::apply_wallpaper_to_displays(
                                        &state2, &wp_id, &ids,
                                    )
                                    .await
                                    {
                                        log::warn!(
                                            "wallpaper recall failed for {wp_id}: {e:#}"
                                        );
                                    }
                                });
                            }
                        }
                    }
                }
            }
        },
    );
}

fn record(
    state: &Arc<AppState>,
    pending: &mut HashMap<String, (tokio::time::Instant, Vec<scheduler::DisplayId>)>,
    snap: routing::DisplaySnapshot,
    settle: Duration,
    startup: bool,
) {
    let key = snap.instance_id.as_deref().unwrap_or(&snap.name);
    let wp_id = if startup {
        state.settings.startup_last_wallpaper(key)
    } else {
        state.settings.resolved_last_wallpaper(key)
    };
    let Some(wp_id) = wp_id else {
        return;
    };
    let entry = pending
        .entry(wp_id)
        .or_insert_with(|| (tokio::time::Instant::now() + settle, Vec::new()));
    entry.1.push(snap.id);
}
