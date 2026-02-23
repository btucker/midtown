use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MatrixBridgeState {
    pub rooms: HashMap<String, String>,
    pub users: HashMap<String, String>,
    #[serde(rename = "last_synced")]
    pub last_synced: HashMap<String, String>,
    #[serde(default)]
    pub matrix_events: HashMap<String, String>,
}

impl MatrixBridgeState {
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| format!("Failed to write {}: {e}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
}
