use crate::daemon_v2::events::DomainEvent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ChannelIndex {}

impl ChannelIndex {
    pub fn apply(&mut self, _event: &DomainEvent) {}
}
