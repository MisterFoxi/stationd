//! Transport-free client DTOs and request generations shared by providers.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StationStatus {
    pub station_name: String,
    pub uptime_seconds: u64,
    pub pid: u32,
    pub timezone: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StatusResponse {
    Connected { status: StationStatus },
    Unavailable { code: String, message: String },
}

#[derive(Clone, Default)]
pub enum RequestState<T> {
    #[default]
    Idle,
    Pending,
    Ready(T),
    Failed(String),
}

/// A single in-flight operation. Cancellation invalidates late callbacks.
pub struct RequestSlot<T> {
    generation: u64,
    pub state: RequestState<T>,
}

impl<T> Default for RequestSlot<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            state: RequestState::Idle,
        }
    }
}

impl<T> RequestSlot<T> {
    pub fn begin(&mut self) -> Option<u64> {
        if matches!(self.state, RequestState::Pending) {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.state = RequestState::Pending;
        Some(self.generation)
    }

    pub fn finish(&mut self, generation: u64, result: Result<T, String>) -> bool {
        if generation != self.generation || !matches!(self.state, RequestState::Pending) {
            return false;
        }
        self.state = match result {
            Ok(value) => RequestState::Ready(value),
            Err(error) => RequestState::Failed(error),
        };
        true
    }

    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.state = RequestState::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_and_stale_results_are_rejected() {
        let mut slot = RequestSlot::default();
        let old = slot.begin().unwrap();
        assert!(slot.begin().is_none());
        slot.cancel();
        let current = slot.begin().unwrap();
        assert!(!slot.finish(old, Ok(10)));
        assert!(matches!(slot.state, RequestState::Pending));
        assert!(slot.finish(current, Ok(20)));
        assert!(!slot.finish(current, Ok(30)));
        assert!(matches!(slot.state, RequestState::Ready(20)));
    }
}

pub mod config;

pub mod manifest;
pub mod registry;
pub mod overview;
