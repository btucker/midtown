//! Plugin state management.
//!
//! Holds the current dashboard data, UI selection state, and view mode.

use midtown_types::{
    ChannelMessage, CoworkerStreamOutput, CoworkerSummary, DashboardState, StreamEvent, TaskSummary,
};

/// Which view the plugin is currently showing.
#[derive(Clone, Debug)]
pub enum View {
    /// Main dashboard with tasks and coworkers.
    Main,
    /// Read-only stream view of a specific coworker.
    CoworkerStream { name: String },
}

/// Section in the main view that can be selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Tasks,
    Coworkers,
}

/// Complete plugin state.
pub struct PluginState {
    pub view: View,

    // Dashboard data
    pub tasks: Vec<TaskSummary>,
    pub coworkers: Vec<CoworkerSummary>,
    pub channel_messages: Vec<ChannelMessage>,
    pub lead_nudges: Vec<String>,
    pub daemon_version: Option<String>,

    // Selection state (main view)
    pub section: Section,
    pub task_index: usize,
    pub coworker_index: usize,

    // Coworker stream data
    pub stream_events: Vec<StreamEvent>,
    pub stream_coworker: Option<String>,
    pub stream_scroll_offset: usize,

    // Error state
    pub error: Option<String>,

    /// Whether we've received at least one dashboard response.
    pub connected: bool,
}

impl Default for PluginState {
    fn default() -> Self {
        Self {
            view: View::Main,
            tasks: Vec::new(),
            coworkers: Vec::new(),
            channel_messages: Vec::new(),
            lead_nudges: Vec::new(),
            daemon_version: None,
            section: Section::Tasks,
            task_index: 0,
            coworker_index: 0,
            stream_events: Vec::new(),
            stream_coworker: None,
            stream_scroll_offset: 0,
            error: None,
            connected: false,
        }
    }
}

impl PluginState {
    /// Update state from a dashboard response.
    pub fn update_dashboard(&mut self, dashboard: DashboardState) {
        self.connected = true;
        self.tasks = dashboard.tasks;
        self.coworkers = dashboard.coworkers;
        self.channel_messages = dashboard.channel_messages;
        self.lead_nudges = dashboard.lead_nudge_queue;
        self.daemon_version = Some(dashboard.daemon_version);

        // Clamp selection indices
        if !self.tasks.is_empty() && self.task_index >= self.tasks.len() {
            self.task_index = self.tasks.len() - 1;
        }
        if !self.coworkers.is_empty() && self.coworker_index >= self.coworkers.len() {
            self.coworker_index = self.coworkers.len() - 1;
        }
    }

    /// Update state from a coworker stream response.
    pub fn update_coworker_stream(&mut self, stream: CoworkerStreamOutput) {
        self.stream_coworker = Some(stream.coworker_name);
        self.stream_events = stream.events;
    }

    /// Move selection up or down in the main view.
    pub fn move_selection(&mut self, delta: i32) {
        match self.section {
            Section::Tasks => {
                let total = self.total_selectable_items();
                if total == 0 {
                    return;
                }
                let current = self.task_index as i32;
                let new_index = (current + delta).clamp(0, total as i32 - 1) as usize;

                // If moving past tasks section into coworkers
                if new_index >= self.tasks.len() {
                    self.section = Section::Coworkers;
                    self.coworker_index = new_index - self.tasks.len();
                } else {
                    self.task_index = new_index;
                }
            }
            Section::Coworkers => {
                let total = self.total_selectable_items();
                if total == 0 {
                    return;
                }
                let current = (self.tasks.len() + self.coworker_index) as i32;
                let new_index = (current + delta).clamp(0, total as i32 - 1) as usize;

                if new_index < self.tasks.len() {
                    self.section = Section::Tasks;
                    self.task_index = new_index;
                } else {
                    self.coworker_index = new_index - self.tasks.len();
                }
            }
        }
    }

    /// Total number of selectable items across both sections.
    fn total_selectable_items(&self) -> usize {
        self.tasks.len() + self.coworkers.len()
    }

    /// Get the global selection index (across both sections).
    pub fn global_selection_index(&self) -> usize {
        match self.section {
            Section::Tasks => self.task_index,
            Section::Coworkers => self.tasks.len() + self.coworker_index,
        }
    }

    /// Get the name of the currently selected coworker (if in coworkers section).
    pub fn selected_coworker_name(&self) -> Option<String> {
        if self.section == Section::Coworkers && self.coworker_index < self.coworkers.len() {
            Some(self.coworkers[self.coworker_index].name.clone())
        } else {
            None
        }
    }

    /// Scroll the stream view up.
    pub fn stream_scroll_up(&mut self) {
        if self.stream_scroll_offset > 0 {
            self.stream_scroll_offset -= 1;
        }
    }

    /// Scroll the stream view down.
    pub fn stream_scroll_down(&mut self) {
        if !self.stream_events.is_empty() {
            self.stream_scroll_offset += 1;
            // Clamp handled during render
        }
    }
}
