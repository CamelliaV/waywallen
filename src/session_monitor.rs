//! Session-level autopause monitor.
//!
//! Runs as a background task and watches two D-Bus signals:
//!
//! * **Lock screen** — `org.freedesktop.ScreenSaver.ActiveChanged(bool)`
//!   on the session bus.  Provided by kscreenlocker on KDE Plasma.
//!
//! * **User switch** — `org.freedesktop.login1.Session` `Active` property
//!   changes on the system bus.  When the user switches to another session
//!   via the display manager the current session's `Active` property becomes
//!   `false`.
//!
//! Each signal is monitored in its own sub-task; a shared channel delivers
//! [`SessionEvent`] values to the main loop, which calls
//! [`Router::update_session_state`].  All D-Bus errors are treated as
//! non-fatal: if a service is unavailable the corresponding sub-task exits
//! and logs a warning, while the rest of the daemon (and the other trigger)
//! continues unaffected.

use std::sync::Arc;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::watch;
use zbus::proxy;

use crate::routing::Router;

// ---------------------------------------------------------------------------
// D-Bus proxy definitions
// ---------------------------------------------------------------------------

#[proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/org/freedesktop/ScreenSaver"
)]
trait ScreenSaver {
    /// Fired when the screensaver / lock screen is activated or deactivated.
    #[zbus(signal)]
    fn active_changed(&self, new_value: bool) -> zbus::Result<()>;

    /// Returns the current active state synchronously so we can seed the
    /// initial value without waiting for the first signal.
    fn get_active(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
trait Login1Session {
    /// `true` when this session is the currently active VT session.
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
}

// ---------------------------------------------------------------------------
// Internal event type
// ---------------------------------------------------------------------------

enum SessionEvent {
    /// The screen-saver / lock screen changed state.
    Locked(bool),
    /// The login session's `Active` property changed.
    SessionActive(bool),
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Spawn the session monitor.  Returns immediately; monitoring runs in the
/// background until `shutdown` signals `true`.
pub fn spawn(router: Arc<Router>, shutdown: watch::Receiver<bool>) {
    tokio::spawn(async move {
        run(router, shutdown).await;
    });
}

async fn run(router: Arc<Router>, mut shutdown: watch::Receiver<bool>) {
    log::info!("session_monitor: starting");
    let (tx, mut rx) = mpsc::channel::<SessionEvent>(16);

    // --- Screen saver (session bus) ----------------------------------------
    match zbus::Connection::session().await {
        Ok(conn) => {
            log::info!("session_monitor: session bus connected, starting screen-saver monitor");
            let tx2 = tx.clone();
            tokio::spawn(async move {
                monitor_screen_saver(conn, tx2).await;
            });
        }
        Err(e) => {
            log::warn!("session_monitor: cannot connect to D-Bus session bus: {e}");
        }
    }

    // --- Login session (system bus) ----------------------------------------
    match zbus::Connection::system().await {
        Ok(conn) => {
            log::info!("session_monitor: system bus connected, starting login-session monitor");
            let tx2 = tx.clone();
            tokio::spawn(async move {
                monitor_login_session(conn, tx2).await;
            });
        }
        Err(e) => {
            log::warn!("session_monitor: cannot connect to D-Bus system bus: {e}");
        }
    }

    // --- Main dispatch loop -------------------------------------------------
    loop {
        tokio::select! {
            // Watch for shutdown signal.
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            // Receive events from sub-tasks and forward to router.
            Some(event) = rx.recv() => {
                match event {
                    SessionEvent::Locked(active) => {
                        log::info!("session_monitor: screen lock active={active}");
                        router.update_session_state(Some(active), None).await;
                    }
                    SessionEvent::SessionActive(active) => {
                        // session_inactive is the inverse of `Active`
                        log::info!("session_monitor: login session active={active}");
                        router.update_session_state(None, Some(!active)).await;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Screen saver sub-task
// ---------------------------------------------------------------------------

async fn monitor_screen_saver(conn: zbus::Connection, tx: mpsc::Sender<SessionEvent>) {
    log::info!(
        "session_monitor: subscribing to org.freedesktop.ScreenSaver \
         at /org/freedesktop/ScreenSaver"
    );
    let proxy = match ScreenSaverProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            log::warn!("session_monitor: ScreenSaver proxy unavailable: {e}");
            return;
        }
    };

    // Seed initial state.
    match proxy.get_active().await {
        Ok(active) => {
            log::info!("session_monitor: ScreenSaver initial state: active={active}");
            let _ = tx.send(SessionEvent::Locked(active)).await;
        }
        Err(e) => {
            // Service may not be running yet (screen not locked on startup).
            log::info!("session_monitor: ScreenSaver.GetActive not available yet: {e}");
        }
    }

    // Subscribe to future changes.
    let mut stream = match proxy.receive_active_changed().await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("session_monitor: ScreenSaver.ActiveChanged subscribe failed: {e}");
            return;
        }
    };

    log::info!("session_monitor: listening for ScreenSaver.ActiveChanged signals");

    while let Some(signal) = stream.next().await {
        match signal.args() {
            Ok(args) => {
                log::info!(
                    "session_monitor: ScreenSaver.ActiveChanged new_value={}",
                    args.new_value()
                );
                let _ = tx.send(SessionEvent::Locked(*args.new_value())).await;
            }
            Err(e) => {
                log::warn!("session_monitor: bad ActiveChanged args: {e}");
            }
        }
    }

    log::info!("session_monitor: ScreenSaver signal stream ended (service stopped?)");
}

// ---------------------------------------------------------------------------
// Login session sub-task
// ---------------------------------------------------------------------------

async fn monitor_login_session(conn: zbus::Connection, tx: mpsc::Sender<SessionEvent>) {
    // Derive session path from $XDG_SESSION_ID — avoids the privileged
    // GetSessionByPID call that fails with AccessDenied for user processes.
    let session_id = match std::env::var("XDG_SESSION_ID") {
        Ok(id) if !id.is_empty() => id,
        _ => {
            log::warn!("session_monitor: $XDG_SESSION_ID not set; user-switch detection disabled");
            return;
        }
    };
    let session_path_str = format!("/org/freedesktop/login1/session/{session_id}");
    log::info!("session_monitor: login1 session path: {session_path_str}");

    let session_builder = match Login1SessionProxy::builder(&conn).path(session_path_str) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("session_monitor: login1 Session path invalid: {e}");
            return;
        }
    };
    let session = match session_builder.build().await {
        Ok(p) => p,
        Err(e) => {
            log::warn!("session_monitor: login1 Session proxy unavailable: {e}");
            return;
        }
    };

    // Seed initial state.
    match session.active().await {
        Ok(active) => {
            log::info!("session_monitor: login1 Session initial Active={active}");
            let _ = tx.send(SessionEvent::SessionActive(active)).await;
        }
        Err(e) => {
            log::warn!("session_monitor: Session.Active initial read failed: {e}");
        }
    }

    // Subscribe to property changes.
    let mut stream = session.receive_active_changed().await;

    log::info!("session_monitor: listening for login1 Session.Active changes");

    while let Some(change) = stream.next().await {
        match change.get().await {
            Ok(active) => {
                log::info!("session_monitor: login1 Session.Active changed to {active}");
                let _ = tx.send(SessionEvent::SessionActive(active)).await;
            }
            Err(e) => {
                log::warn!("session_monitor: Session.Active read after change failed: {e}");
            }
        }
    }

    log::info!("session_monitor: login1 Active property stream ended");
}
