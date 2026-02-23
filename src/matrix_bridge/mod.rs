use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct MatrixBridgeConfig {
    pub matrix_dir: PathBuf,
}

impl MatrixBridgeConfig {
    pub fn new(matrix_dir: PathBuf) -> Self {
        Self { matrix_dir }
    }
}

impl Default for MatrixBridgeConfig {
    fn default() -> Self {
        Self {
            matrix_dir: crate::paths::midtown_base_dir().join("matrix"),
        }
    }
}

/// Launch the matrix bridge runtime.
///
/// Spike phase: placeholder loop that owns lifecycle until process termination.
pub fn run(_config: MatrixBridgeConfig) -> Result<(), String> {
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
