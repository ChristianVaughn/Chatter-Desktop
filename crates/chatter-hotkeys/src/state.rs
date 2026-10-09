//! Platform-independent press/release state shared between a backend's event thread and the
//! public [`HotkeyWatcher`](crate::HotkeyWatcher).
//!
//! Backends never see the user callback: they report "this input went down/up" through
//! [`Core::input`], which only forwards an edge when the input is the configured target. Edges
//! are queued (in order, under the lock) to a dispatcher thread that runs the user callback, so
//! a slow callback can't stall an OS hook and a callback may call back into the watcher.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) struct Core {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Backend-specific encoding of the bound input; `None` = watch nothing.
    target: Option<u32>,
    pressed: bool,
    tx: Option<Sender<bool>>,
}

impl Inner {
    fn set_pressed(&mut self, down: bool) {
        if self.pressed != down {
            self.pressed = down;
            if let Some(tx) = &self.tx {
                let _ = tx.send(down);
            }
        }
    }
}

impl Core {
    pub(crate) fn new() -> (Arc<Core>, Receiver<bool>) {
        let (tx, rx) = mpsc::channel();
        let core = Core {
            inner: Mutex::new(Inner {
                target: None,
                pressed: false,
                tx: Some(tx),
            }),
        };
        (Arc::new(core), rx)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces the target. Emits a release first if the old target was held, so a binding
    /// change can never leave the mic open.
    pub(crate) fn set_target(&self, target: Option<u32>) {
        let mut inner = self.lock();
        inner.set_pressed(false);
        inner.target = target;
    }

    /// Reports an input edge. `matches` is called (under the lock) with the current target and
    /// decides whether this event is the bound input. Nothing is recorded for other inputs.
    pub(crate) fn input(&self, down: bool, matches: impl FnOnce(u32) -> bool) {
        let mut inner = self.lock();
        let Some(target) = inner.target else { return };
        if matches(target) {
            inner.set_pressed(down);
        }
    }

    /// Final release + closes the dispatcher channel. Later inputs are ignored.
    pub(crate) fn shutdown(&self) {
        let mut inner = self.lock();
        inner.set_pressed(false);
        inner.target = None;
        inner.tx = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(rx: &Receiver<bool>) -> Vec<bool> {
        rx.try_iter().collect()
    }

    #[test]
    fn dedupes_and_filters() {
        let (core, rx) = Core::new();
        core.input(true, |_| true); // no target yet
        assert!(drain(&rx).is_empty());

        core.set_target(Some(7));
        core.input(true, |t| t == 8); // other key
        core.input(true, |t| t == 7);
        core.input(true, |t| t == 7); // auto-repeat
        core.input(false, |t| t == 8);
        core.input(false, |t| t == 7);
        core.input(false, |t| t == 7);
        assert_eq!(drain(&rx), vec![true, false]);
    }

    #[test]
    fn retarget_while_held_releases() {
        let (core, rx) = Core::new();
        core.set_target(Some(1));
        core.input(true, |t| t == 1);
        core.set_target(Some(2));
        core.input(false, |t| t == 1); // old key released later: ignored
        core.set_target(None);
        assert_eq!(drain(&rx), vec![true, false]);
    }

    #[test]
    fn shutdown_releases_and_closes() {
        let (core, rx) = Core::new();
        core.set_target(Some(1));
        core.input(true, |t| t == 1);
        core.shutdown();
        core.set_target(Some(1));
        core.input(true, |t| t == 1);
        assert_eq!(rx.iter().collect::<Vec<_>>(), vec![true, false]);
    }
}
