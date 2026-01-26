//! Application state and logic for the chat TUI

use midtown::{Channel, Message};

/// Application state
pub struct App {
    /// All messages from the channel
    pub messages: Vec<Message>,
    /// Current scroll offset (0 = most recent at bottom)
    pub scroll_offset: usize,
    /// Visible height for chat panel (updated during render)
    pub visible_height: usize,
    /// Channel for reading messages
    channel: Option<Channel>,
    /// Last known message count (for detecting new messages)
    last_count: usize,
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
            channel,
            last_count: 0,
        };

        // Initial load
        app.refresh();
        app
    }

    /// Refresh messages from the channel
    pub fn refresh(&mut self) {
        // Read all messages from channel
        if let Some(ref channel) = self.channel
            && let Ok(messages) = channel.read_all()
        {
            let new_count = messages.len();

            // Update messages if count changed
            if new_count != self.last_count {
                let added = new_count.saturating_sub(self.last_count);
                let was_at_bottom = self.scroll_offset == 0;

                self.messages = messages;
                self.last_count = new_count;

                if was_at_bottom {
                    // User was at bottom - stay at bottom (auto-scroll)
                    self.scroll_offset = 0;
                } else {
                    // User had scrolled up - adjust offset to stay viewing same messages
                    self.scroll_offset += added;
                }
            }
        }
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
