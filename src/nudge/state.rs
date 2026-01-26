//! Nudge state tracking per coworker

use std::collections::HashMap;
use std::time::SystemTime;

/// State for tracking nudges to a single coworker
#[derive(Debug, Clone)]
pub struct CoworkerNudgeState {
    /// Coworker identifier
    pub coworker: String,
    /// Time of last nudge
    pub last_nudge: SystemTime,
    /// Total number of nudges sent
    pub nudge_count: u64,
}

impl CoworkerNudgeState {
    /// Create a new state record for a coworker
    pub fn new(coworker: impl Into<String>) -> Self {
        Self {
            coworker: coworker.into(),
            last_nudge: SystemTime::now(),
            nudge_count: 1,
        }
    }

    /// Record a new nudge
    pub fn record_nudge(&mut self) {
        self.last_nudge = SystemTime::now();
        self.nudge_count += 1;
    }

    /// Get the time since last nudge
    pub fn time_since_last_nudge(&self) -> std::time::Duration {
        SystemTime::now()
            .duration_since(self.last_nudge)
            .unwrap_or(std::time::Duration::ZERO)
    }
}

/// Tracker for all coworker nudge states
#[derive(Debug, Clone, Default)]
pub struct NudgeTracker {
    states: HashMap<String, CoworkerNudgeState>,
}

impl NudgeTracker {
    /// Create a new empty tracker
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Get the state for a coworker, if it exists
    pub fn get(&self, coworker: &str) -> Option<&CoworkerNudgeState> {
        self.states.get(coworker)
    }

    /// Record a nudge for a coworker
    pub fn record_nudge(&mut self, coworker: &str) {
        match self.states.get_mut(coworker) {
            Some(state) => state.record_nudge(),
            None => {
                self.states
                    .insert(coworker.to_string(), CoworkerNudgeState::new(coworker));
            }
        }
    }

    /// Get all tracked coworkers
    pub fn coworkers(&self) -> impl Iterator<Item = &str> {
        self.states.keys().map(String::as_str)
    }

    /// Get the number of tracked coworkers
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Check if tracker is empty
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }

    /// Remove a coworker from tracking
    pub fn remove(&mut self, coworker: &str) -> Option<CoworkerNudgeState> {
        self.states.remove(coworker)
    }

    /// Clear all tracking state
    pub fn clear(&mut self) {
        self.states.clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_coworker_state_creation() {
        let state = CoworkerNudgeState::new("polecat1");
        assert_eq!(state.coworker, "polecat1");
        assert_eq!(state.nudge_count, 1);
    }

    #[test]
    fn test_coworker_state_record_nudge() {
        let mut state = CoworkerNudgeState::new("polecat1");
        assert_eq!(state.nudge_count, 1);

        state.record_nudge();
        assert_eq!(state.nudge_count, 2);

        state.record_nudge();
        assert_eq!(state.nudge_count, 3);
    }

    #[test]
    fn test_time_since_last_nudge() {
        let state = CoworkerNudgeState::new("polecat1");
        // Should be very small (just created)
        assert!(state.time_since_last_nudge() < Duration::from_secs(1));
    }

    #[test]
    fn test_tracker_new() {
        let tracker = NudgeTracker::new();
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);
    }

    #[test]
    fn test_tracker_record_and_get() {
        let mut tracker = NudgeTracker::new();

        // First nudge creates state
        tracker.record_nudge("polecat1");
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get("polecat1").is_some());
        assert_eq!(tracker.get("polecat1").unwrap().nudge_count, 1);

        // Second nudge increments count
        tracker.record_nudge("polecat1");
        assert_eq!(tracker.get("polecat1").unwrap().nudge_count, 2);

        // Different coworker
        tracker.record_nudge("polecat2");
        assert_eq!(tracker.len(), 2);
        assert_eq!(tracker.get("polecat2").unwrap().nudge_count, 1);
    }

    #[test]
    fn test_tracker_coworkers() {
        let mut tracker = NudgeTracker::new();
        tracker.record_nudge("alice");
        tracker.record_nudge("bob");
        tracker.record_nudge("charlie");

        let coworkers: Vec<&str> = tracker.coworkers().collect();
        assert_eq!(coworkers.len(), 3);
        assert!(coworkers.contains(&"alice"));
        assert!(coworkers.contains(&"bob"));
        assert!(coworkers.contains(&"charlie"));
    }

    #[test]
    fn test_tracker_remove() {
        let mut tracker = NudgeTracker::new();
        tracker.record_nudge("polecat1");
        tracker.record_nudge("polecat2");

        let removed = tracker.remove("polecat1");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().coworker, "polecat1");
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get("polecat1").is_none());
    }

    #[test]
    fn test_tracker_clear() {
        let mut tracker = NudgeTracker::new();
        tracker.record_nudge("polecat1");
        tracker.record_nudge("polecat2");
        assert_eq!(tracker.len(), 2);

        tracker.clear();
        assert!(tracker.is_empty());
    }
}
