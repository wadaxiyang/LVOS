use std::{error::Error, fmt};

pub trait UiDispatcher: Clone + Send + Sync + 'static {
    /// Queues work for the UI event-loop thread.
    ///
    /// # Errors
    /// Returns [`UiDispatchError`] when the event loop is not running or has terminated.
    fn dispatch(&self, callback: impl FnOnce() + Send + 'static) -> Result<(), UiDispatchError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SlintUiDispatcher;

impl UiDispatcher for SlintUiDispatcher {
    fn dispatch(&self, callback: impl FnOnce() + Send + 'static) -> Result<(), UiDispatchError> {
        slint::invoke_from_event_loop(callback).map_err(|_| UiDispatchError::EventLoopUnavailable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiDispatchError {
    EventLoopUnavailable,
}

impl fmt::Display for UiDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the UI event loop is unavailable")
    }
}

impl Error for UiDispatchError {}
