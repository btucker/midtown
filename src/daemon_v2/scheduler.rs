#[path = "scheduler_tests.rs"]
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::projections::Projections;

/// A boxed decision function: given projections and a channel key, returns commands.
type DecisionFn = Box<dyn Fn(&Projections, &str) -> Vec<Command> + Send + Sync>;

/// A registered decision that is due to run.
pub struct DueDecision<'a> {
    pub name: &'static str,
    pub run: &'a DecisionFn,
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

    /// Register a channel-aware decision function.
    pub fn register(
        &mut self,
        name: &'static str,
        interval: Duration,
        run: impl Fn(&Projections, &str) -> Vec<Command> + Send + Sync + 'static,
    ) {
        self.entries.push(Entry {
            name,
            interval,
            run: Box::new(run),
            last_ran: None,
        });
    }

    /// Register a global decision function that ignores the channel argument.
    pub fn register_global(
        &mut self,
        name: &'static str,
        interval: Duration,
        run: impl Fn(&Projections) -> Vec<Command> + Send + Sync + 'static,
    ) {
        self.entries.push(Entry {
            name,
            interval,
            run: Box::new(move |proj, _channel| run(proj)),
            last_ran: None,
        });
    }

    /// Return all decisions that are currently due to run.
    ///
    /// A decision is due if it has never run, or if enough time has elapsed
    /// since it last ran. Results are returned in ascending interval order
    /// (shortest interval first).
    pub fn due_decisions(&self, now: Instant) -> Vec<DueDecision<'_>> {
        let mut due: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|e| match e.last_ran {
                None => true,
                Some(last) => now.duration_since(last) >= e.interval,
            })
            .collect();

        // Sort by interval ascending so shorter intervals run first
        due.sort_by_key(|e| e.interval);

        due.into_iter()
            .map(|e| DueDecision {
                name: e.name,
                run: &e.run,
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
