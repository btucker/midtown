use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[path = "cooldowns_tests.rs"]
#[cfg(test)]
mod tests;

/// Categories of rate-limited operations in the daemon tick loop.
///
/// Each variant carries a fixed cooldown duration. The [`CooldownTracker`]
/// pairs a category with a string key (typically an agent or task ID) so the
/// same category can cool down independently for different entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CooldownCategory {
    /// Prevents repeated spawn attempts for agents that have no matching task (60 s).
    /// Keyed by agent name.
    OrphanSpawn,
    /// Global throttle on dispatching new agent sessions (30 s).
    /// Keyed by task ID.
    AgentDispatch,
    /// Back-off after a spawn attempt fails (120 s).
    /// Keyed by task ID or channel name.
    SpawnFailure,
    /// Limits how often an agent is nudged to merge or rebase its PR (1 h).
    /// Keyed by agent ID.
    MergeRebaseNudge,
    /// Back-off after a rebase introduces test regressions (1 h).
    /// Keyed by agent ID.
    RebaseRegression,
    /// Rate-limits worktree freshness checks for the lead session (5 min).
    /// Keyed by lead identifier.
    LeadWorktreeFreshness,
    /// Throttles periodic nudges sent to agents about their tasks (1 h).
    /// Keyed by agent ID or a reviewer-escalation key.
    TaskNudge,
    /// Controls how often stale-note checks fire (1 h).
    /// Keyed by note identifier.
    NoteStaleness,
}

impl CooldownCategory {
    pub fn duration(&self) -> Duration {
        match self {
            Self::OrphanSpawn => Duration::from_secs(60),
            Self::AgentDispatch => Duration::from_secs(30),
            Self::SpawnFailure => Duration::from_secs(120),
            Self::MergeRebaseNudge => Duration::from_secs(3600),
            Self::RebaseRegression => Duration::from_secs(3600),
            Self::LeadWorktreeFreshness => Duration::from_secs(300),
            Self::TaskNudge => Duration::from_secs(3600),
            Self::NoteStaleness => Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CooldownTracker {
    entries: HashMap<(CooldownCategory, String), Instant>,
}

impl CooldownTracker {
    pub fn is_active(&self, category: CooldownCategory, key: &str) -> bool {
        self.entries
            .get(&(category, key.to_string()))
            .map(|t| t.elapsed() < category.duration())
            .unwrap_or(false)
    }

    pub fn record(&mut self, category: CooldownCategory, key: String) {
        self.entries.insert((category, key), Instant::now());
    }
}

impl Serialize for CooldownTracker {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        s.serialize_struct("CooldownTracker", 0)?.end()
    }
}

impl<'de> Deserialize<'de> for CooldownTracker {
    fn deserialize<D: serde::Deserializer<'de>>(_d: D) -> Result<Self, D::Error> {
        let _ = serde::de::IgnoredAny::deserialize(_d);
        Ok(Self::default())
    }
}
