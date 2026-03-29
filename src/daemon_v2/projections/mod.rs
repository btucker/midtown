use super::events::DomainEvent;
use serde::{Deserialize, Serialize};

#[path = "projection_spec_tests.rs"]
#[cfg(test)]
mod spec_tests;

pub mod agents;
pub mod channels;
pub mod cooldowns;
pub mod work;

pub use agents::AgentIndex;
pub use channels::ChannelIndex;
pub use cooldowns::CooldownTracker;
pub use work::WorkIndex;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Projections {
    pub agents: AgentIndex,
    pub work: WorkIndex,
    pub channels: ChannelIndex,
    #[serde(skip)]
    pub cooldowns: CooldownTracker,
}

impl Projections {
    pub fn apply(&mut self, event: &DomainEvent) {
        self.agents.apply(event);
        self.work.apply(event);
        self.channels.apply(event);
    }

    pub fn apply_all(&mut self, events: &[DomainEvent]) {
        for event in events {
            self.apply(event);
        }
    }
}
