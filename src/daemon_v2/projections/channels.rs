use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::daemon_v2::events::DomainEvent;

#[path = "channels_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelSettings {
    pub show_full_lead_output: bool,
    #[serde(default)]
    pub lead_driven: bool,
    /// Subdirectory within the repo this channel focuses on (e.g., "packages/auth").
    /// The lead agent's AGENTS.md/CLAUDE.md is loaded from this directory.
    #[serde(default)]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub archived: bool,
    pub settings: ChannelSettings,
    pub workflow: Option<String>,
    pub thread_count: usize,
    pub last_message_at: Option<DateTime<Utc>>,
    #[serde(default)]
    known_threads: HashSet<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ChannelIndex {
    pub channels: HashMap<String, ChannelMeta>,
    pub read_state: HashMap<String, DateTime<Utc>>,
}

impl ChannelIndex {
    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::MessagePosted {
                channel, thread_id, ..
            } => {
                let meta = self
                    .channels
                    .entry(channel.clone())
                    .or_insert_with(|| ChannelMeta {
                        name: channel.clone(),
                        archived: false,
                        settings: ChannelSettings::default(),
                        workflow: None,
                        thread_count: 0,
                        last_message_at: None,
                        known_threads: HashSet::new(),
                    });
                meta.last_message_at = Some(Utc::now());
                if let Some(parent_id) = thread_id
                    && meta.known_threads.insert(parent_id.clone())
                {
                    meta.thread_count += 1;
                }
            }
            DomainEvent::ChannelLeadDrivenSet {
                channel,
                lead_driven,
            } => {
                self.ensure_channel(channel).settings.lead_driven = *lead_driven;
            }
            DomainEvent::ChannelDirectorySet { channel, directory } => {
                self.ensure_channel(channel).settings.directory = directory.clone();
            }
            _ => {}
        }
    }

    pub fn is_lead_driven(&self, channel: &str) -> bool {
        self.channels
            .get(channel)
            .map(|m| m.settings.lead_driven)
            .unwrap_or(false)
    }

    /// Get the configured subdirectory for a channel (for AGENTS.md loading).
    pub fn channel_directory(&self, channel: &str) -> Option<&str> {
        self.channels
            .get(channel)
            .and_then(|m| m.settings.directory.as_deref())
    }

    fn ensure_channel(&mut self, channel: &str) -> &mut ChannelMeta {
        self.channels
            .entry(channel.to_string())
            .or_insert_with(|| ChannelMeta {
                name: channel.to_string(),
                archived: false,
                settings: ChannelSettings::default(),
                workflow: None,
                thread_count: 0,
                last_message_at: None,
                known_threads: HashSet::new(),
            })
    }
}
