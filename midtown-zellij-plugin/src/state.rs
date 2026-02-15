//! Plugin state management.
//!
//! Holds the current dashboard data, UI selection state, and view mode.

use midtown_types::{
    ChannelMessage, CoworkerStreamOutput, CoworkerSummary, DashboardState, StreamEvent, TaskSummary,
};
use zellij_tile::prelude::PaneId;

/// Which view the plugin is currently showing.
#[derive(Clone, Debug)]
pub enum View {
    /// Main dashboard with tasks and coworkers.
    Main,
    /// Read-only stream view of a specific coworker.
    CoworkerStream { name: String },
    /// A coworker has been attached to an interactive pane.
    CoworkerAttached { name: String },
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
    pub lead_provider: Option<String>,
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

    /// Whether we've received at least one successful dashboard response.
    pub connected: bool,

    /// Number of consecutive dashboard RPC failures.
    pub consecutive_failures: u32,

    // --- Phase 4: Attach/Detach & Nudge Delivery ---
    /// Pane ID of an attached coworker's interactive terminal.
    pub attached_pane_id: Option<u32>,

    /// The Lead's terminal pane ID (discovered from PaneUpdate events).
    /// Used for delivering nudges via `write_chars_to_pane_id`.
    pub lead_pane_id: Option<PaneId>,

    /// Plugin's own pane ID (discovered from PaneUpdate events).
    pub self_pane_id: Option<u32>,
}

impl Default for PluginState {
    fn default() -> Self {
        Self {
            view: View::Main,
            tasks: Vec::new(),
            coworkers: Vec::new(),
            channel_messages: Vec::new(),
            lead_nudges: Vec::new(),
            lead_provider: None,
            daemon_version: None,
            section: Section::Tasks,
            task_index: 0,
            coworker_index: 0,
            stream_events: Vec::new(),
            stream_coworker: None,
            stream_scroll_offset: 0,
            error: None,
            connected: false,
            consecutive_failures: 0,
            attached_pane_id: None,
            lead_pane_id: None,
            self_pane_id: None,
        }
    }
}

impl PluginState {
    /// Update state from a dashboard response.
    pub fn update_dashboard(&mut self, dashboard: DashboardState) {
        self.connected = true;
        self.consecutive_failures = 0;
        self.error = None;
        self.tasks = dashboard.tasks;
        self.coworkers = dashboard.coworkers;
        self.channel_messages = dashboard.channel_messages;
        self.lead_nudges = dashboard.lead_nudge_queue;
        self.lead_provider = Some(dashboard.lead_provider);
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
        // Clamp scroll offset in case the new event list is shorter
        if !self.stream_events.is_empty() {
            let max_offset = self.stream_events.len().saturating_sub(1);
            self.stream_scroll_offset = self.stream_scroll_offset.min(max_offset);
        } else {
            self.stream_scroll_offset = 0;
        }
    }

    /// Record an RPC error.
    pub fn record_error(&mut self, error: String) {
        self.error = Some(error);
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

    /// Get the name of the currently selected coworker (if in coworkers section).
    pub fn selected_coworker_name(&self) -> Option<String> {
        if self.section == Section::Coworkers && self.coworker_index < self.coworkers.len() {
            Some(self.coworkers[self.coworker_index].name.clone())
        } else {
            None
        }
    }

    /// Get the name of the attached coworker, if any.
    pub fn attached_coworker_name(&self) -> Option<&str> {
        match &self.view {
            View::CoworkerAttached { name } => Some(name),
            _ => None,
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
            // Clamp to max possible offset (last event index)
            let max_offset = self.stream_events.len().saturating_sub(1);
            self.stream_scroll_offset = self.stream_scroll_offset.min(max_offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dashboard(tasks: usize, coworkers: usize) -> DashboardState {
        DashboardState {
            tasks: (0..tasks)
                .map(|i| TaskSummary {
                    id: format!("{}", i + 1),
                    subject: format!("Task {}", i + 1),
                    status: "in_progress".to_string(),
                    owner: None,
                    pr_number: None,
                    pr_status: None,
                })
                .collect(),
            coworkers: (0..coworkers)
                .map(|i| CoworkerSummary {
                    name: format!("worker{}", i),
                    status: "developing".to_string(),
                    current_task: None,
                    session_id: None,
                    model: "opus".to_string(),
                    is_alive: true,
                    has_usage_limit: false,
                    has_api_error: false,
                    last_event_at: None,
                })
                .collect(),
            channel_messages: Vec::new(),
            lead_nudge_queue: Vec::new(),
            lead_provider: "claude".to_string(),
            daemon_version: "0.5.4".to_string(),
        }
    }

    #[test]
    fn test_update_dashboard_sets_connected() {
        let mut state = PluginState::default();
        assert!(!state.connected);
        state.update_dashboard(make_dashboard(2, 1));
        assert!(state.connected);
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.error.is_none());
    }

    #[test]
    fn test_update_dashboard_clears_error() {
        let mut state = PluginState::default();
        state.error = Some("old error".to_string());
        state.consecutive_failures = 5;
        state.update_dashboard(make_dashboard(1, 1));
        assert!(state.error.is_none());
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn test_update_dashboard_sets_lead_provider() {
        let mut state = PluginState::default();
        let mut dashboard = make_dashboard(1, 1);
        dashboard.lead_provider = "codex".to_string();
        state.update_dashboard(dashboard);
        assert_eq!(state.lead_provider.as_deref(), Some("codex"));
    }

    #[test]
    fn test_move_selection_across_sections() {
        let mut state = PluginState::default();
        state.update_dashboard(make_dashboard(2, 2));
        assert_eq!(state.section, Section::Tasks);
        assert_eq!(state.task_index, 0);

        // Move down through tasks
        state.move_selection(1);
        assert_eq!(state.section, Section::Tasks);
        assert_eq!(state.task_index, 1);

        // Move into coworkers section
        state.move_selection(1);
        assert_eq!(state.section, Section::Coworkers);
        assert_eq!(state.coworker_index, 0);

        // Move to second coworker
        state.move_selection(1);
        assert_eq!(state.section, Section::Coworkers);
        assert_eq!(state.coworker_index, 1);

        // Can't move past end
        state.move_selection(1);
        assert_eq!(state.coworker_index, 1);

        // Move back into tasks
        state.move_selection(-1);
        state.move_selection(-1);
        assert_eq!(state.section, Section::Tasks);
        assert_eq!(state.task_index, 1);
    }

    #[test]
    fn test_selected_coworker_name() {
        let mut state = PluginState::default();
        state.update_dashboard(make_dashboard(1, 2));

        // In tasks section, no coworker selected
        assert_eq!(state.selected_coworker_name(), None);

        // Move to coworkers section
        state.move_selection(1);
        assert_eq!(state.section, Section::Coworkers);
        assert_eq!(state.selected_coworker_name(), Some("worker0".to_string()));
    }

    #[test]
    fn test_clamp_selection_on_dashboard_update() {
        let mut state = PluginState::default();
        state.update_dashboard(make_dashboard(3, 3));
        state.task_index = 2;
        state.section = Section::Coworkers;
        state.coworker_index = 2;

        // Dashboard update with fewer items -- indices should be clamped
        state.update_dashboard(make_dashboard(1, 1));
        assert_eq!(state.task_index, 0);
        assert_eq!(state.coworker_index, 0);
    }

    #[test]
    fn test_stream_scroll() {
        let mut state = PluginState::default();
        state.stream_events = vec![
            StreamEvent {
                timestamp: chrono::Utc::now(),
                event_type: "test".to_string(),
                content: "event1".to_string(),
            },
            StreamEvent {
                timestamp: chrono::Utc::now(),
                event_type: "test".to_string(),
                content: "event2".to_string(),
            },
        ];

        assert_eq!(state.stream_scroll_offset, 0);
        state.stream_scroll_down();
        assert_eq!(state.stream_scroll_offset, 1);
        state.stream_scroll_up();
        assert_eq!(state.stream_scroll_offset, 0);
        // Can't scroll past 0
        state.stream_scroll_up();
        assert_eq!(state.stream_scroll_offset, 0);
    }

    #[test]
    fn test_stream_scroll_clamped() {
        let mut state = PluginState::default();
        // 5 events; max offset should be 4
        state.stream_events = (0..5)
            .map(|i| StreamEvent {
                timestamp: chrono::Utc::now(),
                event_type: "test".to_string(),
                content: format!("event{}", i),
            })
            .collect();

        // Scroll down many times -- should clamp to max offset (4)
        for _ in 0..100 {
            state.stream_scroll_down();
        }
        assert_eq!(state.stream_scroll_offset, 4);
    }

    #[test]
    fn test_stream_scroll_clamps_on_update() {
        let mut state = PluginState::default();
        // Start with 10 events and scroll to offset 9
        state.stream_events = (0..10)
            .map(|i| StreamEvent {
                timestamp: chrono::Utc::now(),
                event_type: "test".to_string(),
                content: format!("event{}", i),
            })
            .collect();
        for _ in 0..20 {
            state.stream_scroll_down();
        }
        assert_eq!(state.stream_scroll_offset, 9);

        // Update with fewer events -- offset should be clamped
        let stream = CoworkerStreamOutput {
            coworker_name: "test".to_string(),
            events: (0..3)
                .map(|i| StreamEvent {
                    timestamp: chrono::Utc::now(),
                    event_type: "test".to_string(),
                    content: format!("event{}", i),
                })
                .collect(),
        };
        state.update_coworker_stream(stream);
        assert_eq!(state.stream_scroll_offset, 2); // max for 3 events
    }

    #[test]
    fn test_stream_scroll_resets_on_empty_update() {
        let mut state = PluginState::default();
        state.stream_scroll_offset = 10;
        let stream = CoworkerStreamOutput {
            coworker_name: "test".to_string(),
            events: Vec::new(),
        };
        state.update_coworker_stream(stream);
        assert_eq!(state.stream_scroll_offset, 0);
    }

    #[test]
    fn test_attached_coworker_name() {
        let mut state = PluginState::default();
        assert_eq!(state.attached_coworker_name(), None);

        state.view = View::CoworkerAttached {
            name: "park".to_string(),
        };
        assert_eq!(state.attached_coworker_name(), Some("park"));

        state.view = View::Main;
        assert_eq!(state.attached_coworker_name(), None);
    }
}
