use super::events::DomainEvent;
use serde::{Deserialize, Serialize};

#[path = "projection_spec_tests.rs"]
#[cfg(test)]
mod spec_tests;

#[path = "agent_lifecycle_spec_tests.rs"]
#[cfg(test)]
mod agent_lifecycle_spec_tests;

#[path = "cooldown_spec_tests.rs"]
#[cfg(test)]
mod cooldown_spec_tests;

pub mod agents;
pub mod channels;
pub mod cooldowns;
pub mod work;

pub use agents::AgentIndex;
pub use channels::ChannelIndex;
pub use cooldowns::CooldownTracker;
pub use work::WorkIndex;

/// A stored reminder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub trigger: String,
    pub message: String,
    pub cron_expr: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Projections {
    pub agents: AgentIndex,
    pub work: WorkIndex,
    pub channels: ChannelIndex,
    #[serde(skip)]
    pub cooldowns: CooldownTracker,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
}

impl Projections {
    pub fn apply(&mut self, event: &DomainEvent) {
        self.agents.apply(event);
        self.work.apply(event);
        self.channels.apply(event);
        // Reminder events
        match event {
            DomainEvent::ReminderCreated {
                id,
                trigger,
                message,
                cron_expr,
            } => {
                self.reminders.push(Reminder {
                    id: id.clone(),
                    trigger: trigger.clone(),
                    message: message.clone(),
                    cron_expr: cron_expr.clone(),
                });
            }
            DomainEvent::ReminderCancelled { id } => {
                self.reminders.retain(|r| r.id != *id);
            }
            _ => {}
        }
    }

    pub fn apply_all(&mut self, events: &[DomainEvent]) {
        for event in events {
            self.apply(event);
        }
    }
}
