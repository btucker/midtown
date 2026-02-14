//! Midtown Zellij sidebar plugin.
//!
//! Renders a dashboard showing tasks, coworkers, and channel activity.
//! Communicates with the daemon via `midtown plugin` CLI commands using
//! Zellij's `run_command()` API.
//!
//! ## Architecture
//!
//! - `MidtownPlugin` holds all UI state and implements `ZellijPlugin`
//! - Timer fires every 2s → runs `midtown --format json plugin dashboard`
//! - `RunCommandResult` delivers stdout → parsed into `DashboardState`
//! - `render()` draws the sidebar based on current view (main or coworker stream)
//! - Key/Mouse events handle navigation between views

use std::collections::BTreeMap;
use zellij_tile::prelude::*;

use midtown_types::DashboardState;

mod render;
mod state;

use state::{PluginState, View};

// Command context keys for identifying run_command responses
const CMD_DASHBOARD: &str = "dashboard";
const CMD_COWORKER_STREAM: &str = "coworker_stream";
const CMD_ATTACH: &str = "attach";

/// Poll interval for dashboard data (seconds).
const POLL_INTERVAL_SECS: f64 = 2.0;

struct MidtownPlugin {
    state: PluginState,
    rows: usize,
    cols: usize,
}

impl Default for MidtownPlugin {
    fn default() -> Self {
        Self {
            state: PluginState::default(),
            rows: 0,
            cols: 0,
        }
    }
}

register_plugin!(MidtownPlugin);

impl ZellijPlugin for MidtownPlugin {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ChangeApplicationState,
            PermissionType::ReadApplicationState,
            PermissionType::RunCommands,
            PermissionType::MessageAndLaunchOtherPlugins,
        ]);
        subscribe(&[
            EventType::Timer,
            EventType::Key,
            EventType::Mouse,
            EventType::RunCommandResult,
            EventType::CustomMessage,
            EventType::PaneUpdate,
            EventType::TabUpdate,
        ]);
        // Start the polling timer
        set_timeout(POLL_INTERVAL_SECS);
        // Request initial data immediately
        self.request_dashboard();
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Timer(..) => {
                self.request_dashboard();
                if let View::CoworkerStream { ref name } = self.state.view {
                    self.request_coworker_stream(&name.clone());
                }
                set_timeout(POLL_INTERVAL_SECS);
                false // Don't re-render just from timer; wait for data
            }
            Event::RunCommandResult(exit_code, stdout, _stderr, context) => {
                self.handle_command_result(exit_code, stdout, context)
            }
            Event::Key(key) => self.handle_key(key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.rows = rows;
        self.cols = cols;
        render::render(&self.state, rows, cols);
    }
}

impl MidtownPlugin {
    /// Request dashboard data from the daemon.
    fn request_dashboard(&self) {
        let mut context = BTreeMap::new();
        context.insert("cmd".to_string(), CMD_DASHBOARD.to_string());
        run_command(
            &["midtown", "--format", "json", "plugin", "dashboard"],
            context,
        );
    }

    /// Request coworker stream data from the daemon.
    fn request_coworker_stream(&self, name: &str) {
        let mut context = BTreeMap::new();
        context.insert("cmd".to_string(), CMD_COWORKER_STREAM.to_string());
        context.insert("name".to_string(), name.to_string());
        run_command(
            &[
                "midtown",
                "--format",
                "json",
                "plugin",
                "coworker-stream",
                name,
            ],
            context,
        );
    }

    /// Handle the result of a run_command call.
    fn handle_command_result(
        &mut self,
        exit_code: Option<i32>,
        stdout: Vec<u8>,
        context: BTreeMap<String, String>,
    ) -> bool {
        let cmd = context.get("cmd").map(|s| s.as_str()).unwrap_or("");

        if exit_code != Some(0) {
            if cmd == CMD_DASHBOARD {
                self.state.error = Some("Daemon not reachable".to_string());
                return true;
            }
            return false;
        }

        let output = String::from_utf8_lossy(&stdout);

        match cmd {
            CMD_DASHBOARD => {
                match serde_json::from_str::<DashboardState>(&output) {
                    Ok(dashboard) => {
                        self.state.update_dashboard(dashboard);
                        self.state.error = None;
                    }
                    Err(e) => {
                        self.state.error = Some(format!("Parse error: {}", e));
                    }
                }
                true
            }
            CMD_COWORKER_STREAM => {
                if let Ok(stream) =
                    serde_json::from_str::<midtown_types::CoworkerStreamOutput>(&output)
                {
                    self.state.update_coworker_stream(stream);
                }
                true
            }
            CMD_ATTACH => true,
            _ => false,
        }
    }

    /// Handle keyboard input.
    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        // Only handle keys without modifiers for navigation
        if key.has_no_modifiers() {
            match &self.state.view {
                View::Main => self.handle_key_main(&key.bare_key),
                View::CoworkerStream { .. } => self.handle_key_stream(&key.bare_key),
            }
        } else {
            false
        }
    }

    /// Handle keys in the main dashboard view.
    fn handle_key_main(&mut self, key: &BareKey) -> bool {
        match key {
            BareKey::Up | BareKey::Char('k') => {
                self.state.move_selection(-1);
                true
            }
            BareKey::Down | BareKey::Char('j') => {
                self.state.move_selection(1);
                true
            }
            BareKey::Enter | BareKey::Right | BareKey::Char('l') => {
                if let Some(name) = self.state.selected_coworker_name() {
                    let name_clone = name.clone();
                    self.state.view = View::CoworkerStream {
                        name: name_clone.clone(),
                    };
                    self.request_coworker_stream(&name_clone);
                    return true;
                }
                false
            }
            BareKey::Char('a') => {
                if let Some(name) = self.state.selected_coworker_name() {
                    let mut context = BTreeMap::new();
                    context.insert("cmd".to_string(), CMD_ATTACH.to_string());
                    run_command(
                        &["midtown", "--format", "json", "plugin", "attach", &name],
                        context,
                    );
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Handle keys in the coworker stream view.
    fn handle_key_stream(&mut self, key: &BareKey) -> bool {
        match key {
            BareKey::Esc | BareKey::Left | BareKey::Char('h') | BareKey::Char('q') => {
                self.state.view = View::Main;
                true
            }
            BareKey::Up | BareKey::Char('k') => {
                self.state.stream_scroll_up();
                true
            }
            BareKey::Down | BareKey::Char('j') => {
                self.state.stream_scroll_down();
                true
            }
            _ => false,
        }
    }

    /// Handle mouse input.
    fn handle_mouse(&mut self, mouse: Mouse) -> bool {
        match mouse {
            Mouse::ScrollUp(..) => {
                match &self.state.view {
                    View::Main => self.state.move_selection(-1),
                    View::CoworkerStream { .. } => self.state.stream_scroll_up(),
                }
                true
            }
            Mouse::ScrollDown(..) => {
                match &self.state.view {
                    View::Main => self.state.move_selection(1),
                    View::CoworkerStream { .. } => self.state.stream_scroll_down(),
                }
                true
            }
            _ => false,
        }
    }
}
