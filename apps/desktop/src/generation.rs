use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use lvos_core::ContentKey;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct QueryGeneration(u64);

impl QueryGeneration {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
pub struct QueryTicket {
    generation: QueryGeneration,
    cancellation: CancellationToken,
}

impl QueryTicket {
    #[must_use]
    pub const fn generation(&self) -> QueryGeneration {
        self.generation
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

#[derive(Debug)]
pub enum CaptureAdmission {
    Admitted(CapturePermit),
    Busy,
    Debounced,
}

#[derive(Debug)]
pub struct CapturePermit {
    state: Arc<Mutex<CaptureState>>,
}

impl Drop for CapturePermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.capture_active = false;
        }
    }
}

#[derive(Debug)]
pub struct ContentFlightPermit {
    key: ContentKey,
    state: Arc<Mutex<GenerationState>>,
}

impl Drop for ContentFlightPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.in_flight_content.remove(&self.key);
        }
    }
}

#[derive(Debug)]
pub struct CaptureGate {
    debounce: Duration,
    capture: Arc<Mutex<CaptureState>>,
    generation: Arc<Mutex<GenerationState>>,
}

#[derive(Debug, Default)]
struct CaptureState {
    capture_active: bool,
    last_trigger: Option<Instant>,
}

#[derive(Debug)]
struct GenerationState {
    current: QueryGeneration,
    current_cancellation: CancellationToken,
    in_flight_content: HashSet<ContentKey>,
}

impl CaptureGate {
    #[must_use]
    pub fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            capture: Arc::new(Mutex::new(CaptureState::default())),
            generation: Arc::new(Mutex::new(GenerationState {
                current: QueryGeneration(0),
                current_cancellation: CancellationToken::new(),
                in_flight_content: HashSet::new(),
            })),
        }
    }

    #[must_use]
    pub fn admit_capture(&self, now: Instant) -> CaptureAdmission {
        let Ok(mut state) = self.capture.lock() else {
            return CaptureAdmission::Busy;
        };
        if state.capture_active {
            return CaptureAdmission::Busy;
        }
        if state
            .last_trigger
            .is_some_and(|previous| now.saturating_duration_since(previous) < self.debounce)
        {
            return CaptureAdmission::Debounced;
        }
        state.capture_active = true;
        state.last_trigger = Some(now);
        CaptureAdmission::Admitted(CapturePermit {
            state: Arc::clone(&self.capture),
        })
    }

    #[must_use]
    pub fn begin_query(&self) -> QueryTicket {
        let mut state = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.current_cancellation.cancel();
        state.current = QueryGeneration(state.current.0.saturating_add(1));
        state.current_cancellation = CancellationToken::new();
        QueryTicket {
            generation: state.current,
            cancellation: state.current_cancellation.clone(),
        }
    }

    #[must_use]
    pub fn is_current(&self, generation: QueryGeneration) -> bool {
        self.generation
            .lock()
            .map_or_else(|_| false, |state| state.current == generation)
    }

    #[must_use]
    pub fn begin_content_flight(&self, key: ContentKey) -> Option<ContentFlightPermit> {
        let mut state = self.generation.lock().ok()?;
        if !state.in_flight_content.insert(key) {
            return None;
        }
        Some(ContentFlightPermit {
            key,
            state: Arc::clone(&self.generation),
        })
    }
}

impl Default for CaptureGate {
    fn default() -> Self {
        Self::new(Duration::from_millis(250))
    }
}
