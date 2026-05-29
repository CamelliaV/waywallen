//! MPRIS-backed media playback monitor.
//!
//! This module deliberately keeps the D-Bus watcher behind a small
//! playback-state registry so a future PipeWire/PulseAudio backend can
//! feed the same aggregate `any_playing` signal into the router.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use zbus::proxy;

use crate::routing::Router;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";

#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MprisPlayer {
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
}

/// MPRIS playback statuses relevant to pause decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    fn from_mpris(raw: &str) -> Self {
        match raw {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// Accept only well-known MPRIS player service names.
pub fn is_mpris_player_name(name: &str) -> bool {
    name.strip_prefix(MPRIS_PREFIX)
        .is_some_and(|suffix| !suffix.is_empty())
}

/// Per-player playback state with an aggregate view.
#[derive(Debug, Default)]
pub struct MediaPlaybackRegistry {
    players: HashMap<String, PlaybackStatus>,
}

impl MediaPlaybackRegistry {
    pub fn set_player_status(&mut self, name: &str, status: PlaybackStatus) {
        if is_mpris_player_name(name) {
            self.players.insert(name.to_string(), status);
        }
    }

    pub fn remove_player(&mut self, name: &str) {
        self.players.remove(name);
    }

    pub fn any_playing(&self) -> bool {
        self.players
            .values()
            .any(|status| *status == PlaybackStatus::Playing)
    }
}

enum MonitorEvent {
    Status(String, PlaybackStatus),
}

/// Spawn the best-effort MPRIS monitor.
pub fn spawn(router: Arc<Router>, shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        if let Err(e) = run(router, shutdown).await {
            log::warn!("media_monitor: {e}");
        }
    });
}

async fn run(router: Arc<Router>, mut shutdown: watch::Receiver<bool>) -> anyhow::Result<()> {
    log::info!("media_monitor: starting MPRIS watcher");
    let conn = zbus::Connection::session().await?;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let mut owner_stream = dbus.receive_name_owner_changed().await?;
    let (tx, mut rx) = mpsc::channel::<MonitorEvent>(32);
    let mut registry = MediaPlaybackRegistry::default();
    let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut published_any = false;

    for name in dbus.list_names().await? {
        let name = name.to_string();
        if is_mpris_player_name(&name) {
            spawn_player_monitor(&conn, &name, &tx, &mut tasks);
        }
    }

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            Some(event) = rx.recv() => {
                match event {
                    MonitorEvent::Status(name, status) => {
                        registry.set_player_status(&name, status);
                        publish_if_changed(&router, &registry, &mut published_any).await;
                    }
                }
            }
            Some(signal) = owner_stream.next() => {
                let args = match signal.args() {
                    Ok(args) => args,
                    Err(e) => {
                        log::warn!("media_monitor: bad NameOwnerChanged args: {e}");
                        continue;
                    }
                };
                let name = args.name().to_string();
                if !is_mpris_player_name(&name) {
                    continue;
                }
                if args.new_owner().is_some() {
                    spawn_player_monitor(&conn, &name, &tx, &mut tasks);
                } else {
                    if let Some(task) = tasks.remove(&name) {
                        task.abort();
                    }
                    registry.remove_player(&name);
                    publish_if_changed(&router, &registry, &mut published_any).await;
                }
            }
        }
    }

    for task in tasks.into_values() {
        task.abort();
    }
    router.update_media_playing(false).await;
    log::info!("media_monitor: exited");
    Ok(())
}

fn spawn_player_monitor(
    conn: &zbus::Connection,
    name: &str,
    tx: &mpsc::Sender<MonitorEvent>,
    tasks: &mut HashMap<String, tokio::task::JoinHandle<()>>,
) {
    if tasks.contains_key(name) {
        return;
    }
    let conn = conn.clone();
    let name = name.to_string();
    let tx = tx.clone();
    let task_key = name.clone();
    let task_name = name.clone();
    let task = tokio::spawn(async move {
        if let Err(e) = monitor_player(conn, name, tx).await {
            log::warn!("media_monitor: player {task_name}: {e}");
        }
    });
    tasks.insert(task_key, task);
}

async fn monitor_player(
    conn: zbus::Connection,
    name: String,
    tx: mpsc::Sender<MonitorEvent>,
) -> anyhow::Result<()> {
    let proxy = MprisPlayerProxy::builder(&conn)
        .destination(name.as_str())?
        .path(MPRIS_PATH)?
        .build()
        .await?;

    if let Ok(status) = proxy.playback_status().await {
        let _ = tx
            .send(MonitorEvent::Status(
                name.clone(),
                PlaybackStatus::from_mpris(&status),
            ))
            .await;
    }

    let mut stream = proxy.receive_playback_status_changed().await;
    while let Some(changed) = stream.next().await {
        match changed.get().await {
            Ok(status) => {
                let _ = tx
                    .send(MonitorEvent::Status(
                        name.clone(),
                        PlaybackStatus::from_mpris(&status),
                    ))
                    .await;
            }
            Err(e) => log::warn!("media_monitor: PlaybackStatus read failed for {name}: {e}"),
        }
    }

    Ok(())
}

async fn publish_if_changed(
    router: &Arc<Router>,
    registry: &MediaPlaybackRegistry,
    published_any: &mut bool,
) {
    let any = registry.any_playing();
    if any != *published_any {
        *published_any = any;
        router.update_media_playing(any).await;
    }
}
