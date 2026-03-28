#[path = "scheduler_tests.rs"]
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::projections::Projections;

/// A pure decision function: given projections and a channel key, returns commands.
pub type DecisionFn = fn(&Projections, &str) -> Vec<Command>;

/// A registered decision that is due to run.
pub struct DueDecision {
    pub name: &'static str,
    pub run: DecisionFn,
}

struct Entry {
    name: &'static str,
    interval: Duration,
    run: DecisionFn,
    last_ran: Option<Instant>,
}

/// Scheduler tracks registered decision functions and their intervals,
/// returning those that are due to run on each tick.
pub struct Scheduler {
    entries: Vec<Entry>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler { entries: vec![] }
    }

    /// Register a decision function with a given interval.
    pub fn register(&mut self, name: &'static str, interval: Duration, run: DecisionFn) {
        self.entries.push(Entry {
            name,
            interval,
            run,
            last_ran: None,
        });
    }

    /// Return all decisions that are currently due to run.
    ///
    /// A decision is due if it has never run, or if enough time has elapsed
    /// since it last ran. Results are returned in ascending interval order
    /// (shortest interval first).
    pub fn due_decisions(&self, now: Instant) -> Vec<DueDecision> {
        let mut due: Vec<(usize, &Entry)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| match e.last_ran {
                None => true,
                Some(last) => now.duration_since(last) >= e.interval,
            })
            .collect();

        // Sort by interval ascending so shorter intervals run first
        due.sort_by_key(|(_, e)| e.interval);

        due.into_iter()
            .map(|(_, e)| DueDecision {
                name: e.name,
                run: e.run,
            })
            .collect()
    }

    /// Mark a decision as having just run at `now`.
    pub fn mark_ran(&mut self, name: &'static str, now: Instant) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) {
            entry.last_ran = Some(now);
        }
    }

    /// Return the time until the next decision is due, or `None` if there are
    /// no registered decisions.
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        self.entries
            .iter()
            .map(|e| match e.last_ran {
                None => Duration::ZERO,
                Some(last) => {
                    let elapsed = now.duration_since(last);
                    e.interval.saturating_sub(elapsed)
                }
            })
            .min()
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
