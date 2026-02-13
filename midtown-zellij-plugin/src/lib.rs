use std::collections::BTreeMap;
use zellij_tile::prelude::*;

struct MidtownPlugin {
    rows: usize,
    cols: usize,
}

impl Default for MidtownPlugin {
    fn default() -> Self {
        Self { rows: 0, cols: 0 }
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
            EventType::CustomMessage,
            EventType::PaneUpdate,
            EventType::TabUpdate,
        ]);
    }

    fn update(&mut self, _event: Event) -> bool {
        // Return true to trigger re-render
        false
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.rows = rows;
        self.cols = cols;
        println!("Midtown Dashboard");
        println!("─────────────────");
        println!("Plugin loaded. Rows: {}, Cols: {}", rows, cols);
    }
}
