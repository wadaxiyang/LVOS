//! One pending, context-bound UI confirmation; business plans stay with the caller.
use std::sync::{Arc, Mutex};

use slint::ComponentHandle;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
use tokio::sync::oneshot;

use crate::MainWindow;

#[derive(Debug, Default)]
struct State {
    epoch: u64,
    next_id: i32,
    presented: bool,
    pending: Option<(i32, oneshot::Sender<bool>)>,
}

impl State {
    fn begin(&mut self, epoch: u64) -> Option<(i32, oneshot::Receiver<bool>)> {
        if epoch != self.epoch || self.pending.is_some() || self.presented {
            return None;
        }
        self.next_id = self.next_id.checked_add(1)?;
        let (sender, receiver) = oneshot::channel();
        self.pending = Some((self.next_id, sender));
        Some((self.next_id, receiver))
    }

    fn resolve(&mut self, id: i32, accepted: bool) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|(pending, _)| *pending != id)
        {
            return false;
        }
        if let Some((_, sender)) = self.pending.take() {
            let _ = sender.send(accepted);
        }
        true
    }

    fn invalidate(&mut self) {
        self.epoch = self.epoch.saturating_add(1);
        if let Some((_, sender)) = self.pending.take() {
            let _ = sender.send(false);
        }
    }
}

/// A finite confirmation request. No credentials or import plan enter the UI.
#[derive(Debug)]
pub struct ConfirmationRequest {
    pub title: String,
    pub message: String,
    pub primary: String,
    /// 1: current device, 2: device identity, 3: portable import.
    pub focus_target: i32,
    pub device_id: String,
}

#[derive(Clone)]
pub struct ConfirmationBroker {
    state: Arc<Mutex<State>>,
    main: slint::Weak<MainWindow>,
}

impl std::fmt::Debug for ConfirmationBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfirmationBroker").finish_non_exhaustive()
    }
}

impl ConfirmationBroker {
    pub(crate) fn install(main: &MainWindow) -> Self {
        let broker = Self {
            state: Arc::new(Mutex::new(State::default())),
            main: main.as_weak(),
        };
        let resolve = broker.clone();
        main.on_confirmation_resolved(move |id, accepted| resolve.resolve(id, accepted));
        let cancel = broker.clone();
        main.on_confirmation_cancelled(move || cancel.invalidate());
        let visibility = broker.clone();
        main.on_confirmation_visibility_changed(move |shown| {
            if let Ok(mut state) = visibility.state.lock() {
                state.presented = shown;
            }
        });
        let cancel = broker.clone();
        main.window().on_winit_window_event(move |window, event| {
            if matches!(event, WindowEvent::Destroyed | WindowEvent::Occluded(true))
                || (matches!(event, WindowEvent::Focused(false))
                    && window.with_winit_window(|native| native.is_minimized() == Some(true))
                        == Some(true))
            {
                cancel.invalidate();
            }
            EventResult::Propagate
        });
        broker
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.state.lock().map_or(u64::MAX, |state| state.epoch)
    }

    #[must_use]
    pub fn is_current(&self, epoch: u64) -> bool {
        self.state.lock().is_ok_and(|state| state.epoch == epoch)
    }

    #[must_use]
    pub fn is_blocking(&self) -> bool {
        self.state
            .lock()
            .map_or(true, |state| state.presented || state.pending.is_some())
    }

    /// Cancel pending and not-yet-presented work when the account or window changes.
    pub fn invalidate(&self) {
        let id = if let Ok(mut state) = self.state.lock() {
            state.invalidate();
            state.next_id
        } else {
            return;
        };
        let main = self.main.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(main) = main.upgrade()
                && main.get_confirmation_id() == id
            {
                main.set_confirmation_shown(false);
            }
        });
    }

    fn resolve(&self, id: i32, accepted: bool) {
        let resolved = self
            .state
            .lock()
            .is_ok_and(|mut state| state.resolve(id, accepted));
        if resolved {
            let main = self.main.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(main) = main.upgrade()
                    && main.get_confirmation_id() == id
                {
                    main.set_confirmation_shown(false);
                }
            });
        }
    }

    /// Await the user's decision without blocking Slint. Duplicate/stale requests cancel.
    pub async fn confirm(&self, epoch: u64, request: ConfirmationRequest) -> bool {
        let Some((id, receiver)) = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.begin(epoch))
        else {
            return false;
        };
        let _guard = PendingGuard {
            broker: self.clone(),
            id,
        };
        let broker = self.clone();
        if slint::invoke_from_event_loop(move || {
            let pending = broker.state.lock().is_ok_and(|state| {
                state
                    .pending
                    .as_ref()
                    .is_some_and(|(current, _)| *current == id)
            });
            if !pending {
                return;
            }
            let Some(main) = broker
                .main
                .upgrade()
                .filter(|main| main.window().is_visible())
            else {
                broker.resolve(id, false);
                return;
            };
            main.set_confirmation_id(id);
            main.set_confirmation_title(request.title.into());
            main.set_confirmation_message(request.message.into());
            main.set_confirmation_primary(request.primary.into());
            main.set_confirmation_focus_target(request.focus_target);
            main.set_confirmation_device_id(request.device_id.into());
            main.set_confirmation_shown(true);
        })
        .is_err()
        {
            self.resolve(id, false);
        }
        receiver.await.unwrap_or(false) && self.is_current(epoch)
    }
}

struct PendingGuard {
    broker: ConfirmationBroker,
    id: i32,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.broker.resolve(self.id, false);
    }
}

#[cfg(test)]
mod tests {
    use super::State;

    #[test]
    fn duplicate_requests_do_not_replace_the_visible_decision() {
        let mut state = State::default();
        let Some((id, mut result)) = state.begin(0) else {
            unreachable!()
        };
        assert!(state.begin(0).is_none());
        assert!(!state.resolve(id + 1, true));
        assert!(state.resolve(id, true));
        assert!(!state.resolve(id, false));
        assert_eq!(result.try_recv(), Ok(true));
    }

    #[test]
    fn account_or_window_change_cancels_and_rejects_stale_work() {
        let mut state = State::default();
        let Some((old_id, mut result)) = state.begin(0) else {
            unreachable!()
        };
        state.invalidate();
        assert_eq!(result.try_recv(), Ok(false));
        assert!(state.begin(0).is_none());
        let Some((new_id, mut result)) = state.begin(1) else {
            unreachable!()
        };
        assert!(!state.resolve(old_id, true));
        assert!(state.resolve(new_id, false));
        assert_eq!(result.try_recv(), Ok(false));
    }
}
