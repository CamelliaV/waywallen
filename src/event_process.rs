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

use std::sync::Arc;

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
            let mut recall_started = ! restore_last;

            // Already-latched phase markers fire their reaction once.
            if ! recall_started && state.events.is_sources_ready() {
                spawn_wallpaper_recall(state.clone());
                recall_started = true;
            }

            loop {
                match bus.recv().await {
                    Ok(GlobalEvent::SourcesReady) => {
                        if ! recall_started {
                            spawn_wallpaper_recall(state.clone());
                            recall_started = true;
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Some events were dropped. Re-read latches so
                        // missed phase markers still trigger.
                        if ! recall_started && state.events.is_sources_ready() {
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

async fn recall_for_display(state: &Arc<AppState>, snap: &routing::DisplaySnapshot) {
    let key = snap.instance_id.as_deref().unwrap_or(&snap.name);
    let Some(wp_id) = state.settings.resolved_last_wallpaper(key) else {
        return;
    };
    log::info!(
        "wallpaper recall: display id={} key={key} -> wallpaper {wp_id}",
        snap.id
    );
    if let Err(e) = crate::control::apply_wallpaper_to_displays(state, &wp_id, &[snap.id]).await {
        log::warn!(
            "wallpaper recall failed for display id={} key={key}: {e:#}",
            snap.id
        );
    }
}

/// Long-lived watcher: re-apply each display's persisted wallpaper as
/// it becomes visible. Spawned by the dispatcher when `SourcesReady`
/// fires so `source_snapshot` is guaranteed populated when the first
/// apply runs. Single path covers startup + hot-plug.
fn spawn_wallpaper_recall(state: Arc<AppState>) {
    let tasks_h = state.tasks.clone();
    tasks_h.spawn_async(
        tasks::TaskKind::Service,
        "service/wallpaper-recall",
        async move {
            use std::collections::HashSet;
            use std::time::Duration;
            let mut seen: HashSet<scheduler::DisplayId> = HashSet::new();

            // Per-display apply is delayed by SETTLE so a display that
            // just emitted Upsert has time to finalize its geometry /
            // scale / DPMS state before we spawn a renderer for it.
            // Each schedule runs in its own task so multiple displays
            // light up in parallel, not sequentially.
            const SETTLE: Duration = Duration::from_secs(2);
            let schedule = |snap: routing::DisplaySnapshot| {
                let state = state.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(SETTLE).await;
                    recall_for_display(&state, &snap).await;
                });
            };

            for snap in state.router.snapshot_displays().await {
                if seen.insert(snap.id) {
                    schedule(snap);
                }
            }
            let mut events_rx = state.router.subscribe_events();
            loop {
                match events_rx.recv().await {
                    Ok(routing::RouterEvent::DisplayUpsert(snap)) => {
                        if seen.insert(snap.id) {
                            schedule(snap);
                        }
                    }
                    Ok(routing::RouterEvent::DisplaysReplace(list)) => {
                        for snap in list {
                            if seen.insert(snap.id) {
                                schedule(snap);
                            }
                        }
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        for snap in state.router.snapshot_displays().await {
                            if seen.insert(snap.id) {
                                schedule(snap);
                            }
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
