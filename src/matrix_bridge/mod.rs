use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use state::MatrixBridgeState;

pub mod as_server;
pub mod client;
pub mod inbound;
pub mod state;
pub mod sync;

#[derive(Debug, Clone)]
pub struct MatrixBridgeConfig {
    pub matrix_dir: PathBuf,
    pub server_name: String,
    pub matrix_port: u16,
    pub as_port: u16,
}

impl MatrixBridgeConfig {
    pub fn new(matrix_dir: PathBuf) -> Self {
        Self {
            matrix_dir,
            server_name: "midtown.local".to_string(),
            matrix_port: 6167,
            as_port: 47025,
        }
    }
}

impl Default for MatrixBridgeConfig {
    fn default() -> Self {
        Self {
            matrix_dir: crate::paths::midtown_base_dir().join("matrix"),
            server_name: "midtown.local".to_string(),
            matrix_port: 6167,
            as_port: 47025,
        }
    }
}

/// Launch the matrix bridge runtime.
pub fn run(config: MatrixBridgeConfig) -> Result<(), String> {
    std::fs::create_dir_all(&config.matrix_dir).map_err(|e| {
        format!(
            "Failed to create matrix directory {}: {e}",
            config.matrix_dir.display()
        )
    })?;

    write_file_if_missing(
        &config.conduit_config_path(),
        default_conduit_config(&config)?,
    )?;
    write_file_if_missing(
        &config.as_registration_path(),
        default_as_registration_config(&config)?,
    )?;

    let state_path = config.state_path();
    if !state_path.exists() {
        MatrixBridgeState::default().save(&state_path)?;
    }

    start_conduit(&config)?;

    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

impl MatrixBridgeConfig {
    fn conduit_config_path(&self) -> PathBuf {
        self.matrix_dir.join("conduit.toml")
    }

    fn as_registration_path(&self) -> PathBuf {
        self.matrix_dir.join("as-registration.yaml")
    }

    fn state_path(&self) -> PathBuf {
        self.matrix_dir.join("state.json")
    }
}

fn default_conduit_config(config: &MatrixBridgeConfig) -> Result<String, String> {
    let database_path = config
        .matrix_dir
        .join("db")
        .to_str()
        .map(std::string::ToString::to_string)
        .ok_or_else(|| "Database path is not valid UTF-8".to_string())?;

    Ok(format!(
        r#"[global]
server_name = "{}"
database_backend = "rocksdb"
database_path = "{}"
port = {}
allow_registration = false
allow_federation = false
"#,
        config.server_name, database_path, config.matrix_port
    ))
}

fn default_as_registration_config(config: &MatrixBridgeConfig) -> Result<String, String> {
    let as_token = random_token();
    let hs_token = random_token();

    Ok(format!(
        r##"id: midtown-bridge
url: http://localhost:{}
as_token: {}
hs_token: {}
sender_localpart: midtown
namespaces:
  users:
    - exclusive: true
      regex: "@(lexington|park|madison|...):{}"
  rooms: []
  aliases:
    - exclusive: false
      regex: "#.*:{}"
"##,
        config.as_port, as_token, hs_token, config.server_name, config.server_name
    ))
}

fn write_file_if_missing(path: &Path, contents: String) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }

    std::fs::write(path, contents).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

fn start_conduit(config: &MatrixBridgeConfig) -> Result<(), String> {
    let exe = std::env::var("MIDTOWN_CONDUIT_BINARY").unwrap_or_else(|_| {
        config
            .matrix_dir
            .join("bin")
            .join("conduit")
            .to_string_lossy()
            .to_string()
    });

    let mut cmd = Command::new(&exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .arg("--config")
        .arg(config.conduit_config_path());

    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("Failed to spawn Conduit '{exe}': {e}"))
}

fn random_token() -> String {
    format!(
        "{:08x}{:08x}{:08x}",
        fastrand::u32(..),
        fastrand::u32(..),
        fastrand::u32(..)
    )
}
