use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[path = "cooldowns_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CooldownCategory {
    OrphanSpawn,
    AgentDispatch,
    SpawnFailure,
    MergeRebaseNudge,
    RebaseRegression,
    LeadWorktreeFreshness,
    TaskNudge,
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

#[derive(Debug, Default)]
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
