//! Midtown Zellij sidebar plugin.
//!
//! Renders a dashboard showing tasks, coworkers, and channel activity.
//! Communicates with the daemon via `midtown plugin` CLI commands using
//! Zellij's `run_command()` API.
//!
//! ## Architecture
//!
//! - `MidtownPlugin` holds all UI state and implements `ZellijPlugin`
//! - Timer fires every 2s → runs `midtown plugin dashboard`
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
const CMD_DETACH: &str = "detach";

/// Poll interval for dashboard data (seconds).
const POLL_INTERVAL_SECS: f64 = 2.0;

/// Number of consecutive failures before marking daemon as disconnected.
const DISCONNECT_THRESHOLD: u32 = 3;

#[derive(Default)]
struct MidtownPlugin {
    state: PluginState,
    rows: usize,
    cols: usize,
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
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                self.handle_command_result(exit_code, stdout, stderr, context)
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
        run_command(&["midtown", "plugin", "dashboard"], context);
    }

    /// Request coworker stream data from the daemon.
    fn request_coworker_stream(&self, name: &str) {
        let mut context = BTreeMap::new();
        context.insert("cmd".to_string(), CMD_COWORKER_STREAM.to_string());
        context.insert("name".to_string(), name.to_string());
        run_command(&["midtown", "plugin", "coworker-stream", name], context);
    }

    /// Send an attach request for a coworker.
    fn request_attach(&self, name: &str, force: bool) {
        let mut context = BTreeMap::new();
        context.insert("cmd".to_string(), CMD_ATTACH.to_string());
        context.insert("name".to_string(), name.to_string());
        let mut args = vec!["midtown", "plugin", "attach", name];
        if force {
            args.push("--force");
        }
        run_command(&args, context);
    }

    /// Send a detach request for a coworker.
    fn request_detach(&self, name: &str) {
        let mut context = BTreeMap::new();
        context.insert("cmd".to_string(), CMD_DETACH.to_string());
        context.insert("name".to_string(), name.to_string());
        run_command(&["midtown", "plugin", "detach", name], context);
    }

    /// Handle the result of a run_command call.
    fn handle_command_result(
        &mut self,
        exit_code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: BTreeMap<String, String>,
    ) -> bool {
        let cmd = context.get("cmd").map(|s| s.as_str()).unwrap_or("");

        if exit_code != Some(0) {
            let stderr_str = String::from_utf8_lossy(&stderr);
            return self.handle_command_error(cmd, &stderr_str);
        }

        let output = String::from_utf8_lossy(&stdout);

        match cmd {
            CMD_DASHBOARD => {
                match serde_json::from_str::<DashboardState>(&output) {
                    Ok(dashboard) => {
                        self.state.update_dashboard(dashboard);
                    }
                    Err(e) => {
                        self.state.record_error(format!("Parse error: {}", e));
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
            CMD_ATTACH => {
                match serde_json::from_str::<midtown_types::AttachResponse>(&output) {
                    Ok(response) if response.success => {
                        if let Some(session_id) = response.session_id {
                            let cmd = CommandToRun::new_with_args(
                                "claude",
                                vec!["--resume".to_string(), session_id],
                            );
                            open_command_pane(cmd, BTreeMap::new());
                        }
                        if let Some(name) = context.get("name") {
                            self.state.view = View::CoworkerAttached { name: name.clone() };
                        }
                    }
                    Ok(response) => {
                        if let Some(err) = response.error {
                            self.state.record_error(format!("Attach failed: {}", err));
                        }
                    }
                    Err(e) => {
                        self.state
                            .record_error(format!("Attach parse error: {}", e));
                    }
                }
                true
            }
            CMD_DETACH => {
                // On successful detach, return to main view
                self.state.view = View::Main;
                true
            }
            _ => false,
        }
    }

    /// Handle a command error (non-zero exit code).
    fn handle_command_error(&mut self, cmd: &str, stderr: &str) -> bool {
        if cmd == CMD_DASHBOARD {
            self.state.consecutive_failures += 1;
            if self.state.consecutive_failures >= DISCONNECT_THRESHOLD {
                self.state.connected = false;
            }
            let msg = if stderr.is_empty() {
                "Daemon not reachable".to_string()
            } else {
                format!("Daemon error: {}", stderr.lines().next().unwrap_or(""))
            };
            self.state.error = Some(msg);
            return true;
        }
        false
    }

    /// Handle keyboard input.
    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        // Only handle keys without modifiers for navigation
        if key.has_no_modifiers() {
            match &self.state.view {
                View::Main => self.handle_key_main(&key.bare_key),
                View::CoworkerStream { .. } => self.handle_key_stream(&key.bare_key),
                View::CoworkerAttached { .. } => self.handle_key_attached(&key.bare_key),
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
                    self.state.view = View::CoworkerStream { name: name.clone() };
                    self.state.stream_scroll_offset = 0;
                    self.state.stream_events.clear();
                    self.request_coworker_stream(&name);
                    return true;
                }
                false
            }
            BareKey::Char('a') => {
                // Graceful attach to selected coworker
                if let Some(name) = self.state.selected_coworker_name() {
                    self.request_attach(&name, false);
                    return true;
                }
                false
            }
            BareKey::Char('A') => {
                // Force attach to selected coworker
                if let Some(name) = self.state.selected_coworker_name() {
                    self.request_attach(&name, true);
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
                self.state.stream_events.clear();
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
            BareKey::Char('a') => {
                // Attach from stream view
                if let View::CoworkerStream { ref name } = self.state.view {
                    let name = name.clone();
                    self.request_attach(&name, false);
                    return true;
                }
                false
            }
            BareKey::Char('A') => {
                // Force attach from stream view
                if let View::CoworkerStream { ref name } = self.state.view {
                    let name = name.clone();
                    self.request_attach(&name, true);
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// Handle keys in the attached coworker view.
    fn handle_key_attached(&mut self, key: &BareKey) -> bool {
        match key {
            BareKey::Char('d') => {
                // Graceful detach
                if let View::CoworkerAttached { ref name } = self.state.view {
                    let name = name.clone();
                    self.request_detach(&name);
                }
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
                    _ => {}
                }
                true
            }
            Mouse::ScrollDown(..) => {
                match &self.state.view {
                    View::Main => self.state.move_selection(1),
                    View::CoworkerStream { .. } => self.state.stream_scroll_down(),
                    _ => {}
                }
                true
            }
            _ => false,
        }
    }
}
