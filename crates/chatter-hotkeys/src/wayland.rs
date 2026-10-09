//! Linux/Wayland backend: the XDG `org.freedesktop.portal.GlobalShortcuts` portal.
//!
//! Wayland doesn't let clients observe global input, so the compositor does the matching: we
//! register one shortcut (`push-to-talk`) with a *preferred* trigger derived from the binding
//! and get `Activated` / `Deactivated` signals. The desktop decides the real trigger (it may
//! ask the user, and usually remembers the user's choice per app + shortcut id, so a changed
//! preferred trigger may be ignored after the first bind). Mouse buttons can't be expressed as
//! a preferred trigger; the user has to assign them in the desktop's dialog.
//!
//! All portal work happens on one thread running a current-thread tokio runtime with its own
//! D-Bus connection (not ashpd's process-global one, whose background tasks would die with
//! our runtime).

use std::pin::pin;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context};
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::codes;
use crate::state::Core;
use crate::{Backend, Binding, Platform};

const SHORTCUT_ID: &str = "push-to-talk";
const SHORTCUT_DESCRIPTION: &str = "Push to talk";
/// All portal events map to this single target.
const TARGET: u32 = 1;
/// Optional app id to register for non-sandboxed apps (`org.freedesktop.host.portal.Registry`);
/// some portal backends (GNOME) refuse host apps without one. Should match the .desktop file.
const APP_ID_ENV: &str = "CHATTER_HOTKEYS_APP_ID";

enum Cmd {
    Bind(Option<Binding>),
    Stop,
}

pub(crate) struct PortalWatcher {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl PortalWatcher {
    pub(crate) fn start(core: Arc<Core>) -> anyhow::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("chatter-hotkeys-portal".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("tokio runtime: {e}")));
                        return;
                    }
                };
                rt.block_on(run(core, cmd_rx, ready_tx));
            })
            .context("spawning portal thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(PortalWatcher {
                cmd_tx,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join(); // it has already returned
                Err(anyhow!(e))
            }
            // D-Bus is hanging: detach the thread instead of blocking start(). It exits on its
            // own once its pending call returns (the ready send fails, or cmd_rx is closed).
            Err(_) => Err(anyhow!("timed out talking to the desktop portal")),
        }
    }
}

impl Platform for PortalWatcher {
    fn backend(&self) -> Backend {
        Backend::WaylandPortal
    }

    fn resolve(&self, _binding: &Binding) -> Option<u32> {
        // Any binding can be offered to the portal; the DE decides what it really triggers on.
        Some(TARGET)
    }

    fn binding_changed(&self, binding: Option<&Binding>, _target: Option<u32>) {
        let _ = self.cmd_tx.send(Cmd::Bind(binding.cloned()));
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.cmd_tx.send(Cmd::Stop);
            let _ = thread.join();
        }
    }
}

impl Drop for PortalWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

type PortalSession = Session<GlobalShortcuts>;

async fn open() -> Result<(ashpd::zbus::Connection, GlobalShortcuts, PortalSession), String> {
    let conn = ashpd::zbus::Connection::session()
        .await
        .map_err(|e| format!("D-Bus session bus: {e}"))?;
    if let Ok(app_id) = std::env::var(APP_ID_ENV) {
        match ashpd::AppID::try_from(app_id.as_str()) {
            Ok(id) => {
                if let Err(e) = ashpd::register_host_app_with_connection(conn.clone(), id).await {
                    log::warn!("hotkeys: registering app id {app_id:?} with the portal: {e}");
                }
            }
            Err(e) => log::warn!("hotkeys: invalid {APP_ID_ENV}={app_id:?}: {e}"),
        }
    }
    let portal = GlobalShortcuts::with_connection(conn.clone())
        .await
        .map_err(|e| format!("GlobalShortcuts portal: {e}"))?;
    // ashpd treats "service missing" as version 1, so prove the portal works by creating a
    // session (no user interaction; it's reused for the first bind).
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| format!("GlobalShortcuts.CreateSession: {e}"))?;
    Ok((conn, portal, session))
}

async fn run(
    core: Arc<Core>,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    ready_tx: std_mpsc::Sender<Result<(), String>>,
) {
    let (_conn, portal, first_session) = match open().await {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let streams = async {
        (
            portal.receive_activated().await,
            portal.receive_deactivated().await,
        )
    };
    let (activated, deactivated) = match streams.await {
        (Ok(a), Ok(d)) => (a, d),
        (Err(e), _) | (_, Err(e)) => {
            let _ = first_session.close().await;
            let _ = ready_tx.send(Err(format!("subscribing to shortcut signals: {e}")));
            return;
        }
    };
    let mut activated = pin!(activated);
    let mut deactivated = pin!(deactivated);
    if ready_tx.send(Ok(())).is_err() {
        // start() gave up waiting; nobody will ever send Stop through a live watcher.
        let _ = first_session.close().await;
        return;
    }
    drop(ready_tx);

    // A session's shortcuts can only be bound once, so every rebind uses a fresh session.
    let mut session: Option<PortalSession> = Some(first_session);
    let mut session_bound = false;
    let mut pending: Option<Option<Binding>> = None;

    'outer: loop {
        let binding = match pending.take() {
            Some(b) => b,
            None => {
                tokio::select! {
                    cmd = cmd_rx.recv() => match cmd {
                        Some(Cmd::Bind(b)) => b,
                        Some(Cmd::Stop) | None => break 'outer,
                    },
                    Some(ev) = activated.next() => {
                        if ev.shortcut_id() == SHORTCUT_ID {
                            core.input(true, |t| t == TARGET);
                        }
                        continue;
                    }
                    Some(ev) = deactivated.next() => {
                        if ev.shortcut_id() == SHORTCUT_ID {
                            core.input(false, |t| t == TARGET);
                        }
                        continue;
                    }
                }
            }
        };

        if session_bound {
            if let Some(old) = session.take() {
                let _ = old.close().await;
            }
            session_bound = false;
        }
        let Some(binding) = binding else { continue };
        if session.is_none() {
            match portal.create_session(CreateSessionOptions::default()).await {
                Ok(s) => session = Some(s),
                Err(e) => {
                    log::warn!("hotkeys: GlobalShortcuts.CreateSession failed: {e}");
                    continue;
                }
            }
        }
        let Some(current) = session.as_ref() else {
            continue;
        };

        let trigger = codes::portal_trigger(&binding.code);
        let shortcut = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION)
            .preferred_trigger(trigger.as_deref());
        let shortcuts = [shortcut];
        // Even a failed or cancelled bind uses up the session.
        session_bound = true;
        // Binding may show a dialog and wait for the user; stay responsive to Stop/rebind.
        let bind =
            portal.bind_shortcuts(current, &shortcuts, None, BindShortcutsOptions::default());
        let mut bind = pin!(bind);
        tokio::select! {
            res = &mut bind => {
                match res.and_then(|req| req.response()) {
                    Ok(resp) => {
                        let desc: Vec<&str> =
                            resp.shortcuts().iter().map(|s| s.trigger_description()).collect();
                        log::info!(
                            "hotkeys: portal shortcut bound (preferred {trigger:?}, actual {desc:?})"
                        );
                    }
                    Err(e) => log::warn!("hotkeys: GlobalShortcuts.BindShortcuts failed: {e}"),
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                // A newer binding arrived while this one waited on the user.
                Some(Cmd::Bind(b)) => pending = Some(b),
                Some(Cmd::Stop) | None => break 'outer,
            },
        }
    }

    if let Some(s) = session.take() {
        let _ = s.close().await;
    }
}
