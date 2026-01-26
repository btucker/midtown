//! Application state and logic for the chat TUI

use std::collections::HashMap;

use midtown::{Channel, Message, MessageType};

/// Coworker information for the team panel
#[derive(Debug, Clone)]
pub struct CoworkerInfo {
    pub name: String,
    pub last_action: Option<String>,
}

/// Application state
pub struct App {
    /// All messages from the channel
    pub messages: Vec<Message>,
    /// Current scroll offset (0 = most recent at bottom)
    pub scroll_offset: usize,
    /// Visible height for chat panel (updated during render)
    pub visible_height: usize,
    /// Active coworkers with their last /me action
    pub coworkers: Vec<CoworkerInfo>,
    /// Channel for reading messages
    channel: Option<Channel>,
    /// Last known message count (for detecting new messages)
    last_count: usize,
    /// Cache of last actions by coworker name
    last_actions: HashMap<String, String>,
}

impl App {
    pub fn new() -> Self {
        // Determine the repo name from current directory
        let repo_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".to_string());

        let channel = Channel::for_repo(&repo_name).ok();
        let mut app = Self {
            messages: Vec::new(),
            scroll_offset: 0,
            visible_height: 20,
            coworkers: Vec::new(),
            channel,
            last_count: 0,
            last_actions: HashMap::new(),
        };

        // Initial load
        app.refresh();
        app
    }

    /// Refresh messages and coworker state
    pub fn refresh(&mut self) {
        // Read all messages from channel
        if let Some(ref channel) = self.channel
            && let Ok(messages) = channel.read_all()
        {
            let new_count = messages.len();

            // Update messages if count changed
            if new_count != self.last_count {
                self.messages = messages;
                self.last_count = new_count;

                // Update last actions from Action messages
                self.update_last_actions();
            }
        }

        // Update coworker list from daemon (could poll RPC, for now use channel data)
        self.update_coworkers();
    }

    /// Extract last /me actions from messages
    fn update_last_actions(&mut self) {
        self.last_actions.clear();

        for msg in &self.messages {
            if msg.message_type == MessageType::Action {
                self.last_actions
                    .insert(msg.from.clone(), msg.content.clone());
            }
        }
    }

    /// Update coworker list based on message senders
    fn update_coworkers(&mut self) {
        // Build coworker list from message senders (excluding system, github, Lead)
        let mut seen: HashMap<String, bool> = HashMap::new();
        let excluded = ["system", "github", "Lead"];

        for msg in &self.messages {
            if !excluded.contains(&msg.from.as_str()) && !seen.contains_key(&msg.from) {
                seen.insert(msg.from.clone(), true);
            }
        }

        // Convert to CoworkerInfo list
        self.coworkers = seen
            .keys()
            .map(|name| CoworkerInfo {
                name: name.clone(),
                last_action: self.last_actions.get(name).cloned(),
            })
            .collect();

        // Sort by name for consistent display
        self.coworkers.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Scroll up one line
    pub fn scroll_up(&mut self) {
        let max_scroll = self.max_scroll();
        if self.scroll_offset < max_scroll {
            self.scroll_offset += 1;
        }
    }

    /// Scroll down one line
    pub fn scroll_down(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    /// Page up
    pub fn page_up(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        let max_scroll = self.max_scroll();
        self.scroll_offset = (self.scroll_offset + page_size).min(max_scroll);
    }

    /// Page down
    pub fn page_down(&mut self) {
        let page_size = self.visible_height.saturating_sub(2);
        self.scroll_offset = self.scroll_offset.saturating_sub(page_size);
    }

    /// Scroll to top (oldest messages)
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = self.max_scroll();
    }

    /// Scroll to bottom (newest messages)
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    /// Maximum scroll offset
    fn max_scroll(&self) -> usize {
        self.messages.len().saturating_sub(self.visible_height)
    }

    /// Get messages visible in the current scroll position
    pub fn visible_messages(&self) -> &[Message] {
        let total = self.messages.len();
        if total == 0 {
            return &[];
        }

        // scroll_offset=0 means we show the most recent messages (end of list)
        // Higher scroll_offset means we show older messages
        let end = total.saturating_sub(self.scroll_offset);
        let start = end.saturating_sub(self.visible_height);

        &self.messages[start..end]
    }
}
