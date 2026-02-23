use std::collections::BTreeSet;
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
            server_name: "matrix.local".to_string(),
            matrix_port: 6167,
            as_port: 47025,
        }
    }
}

impl Default for MatrixBridgeConfig {
    fn default() -> Self {
        Self {
            matrix_dir: crate::paths::midtown_base_dir().join("matrix"),
            server_name: "matrix.local".to_string(),
            matrix_port: 6167,
            as_port: 47025,
        }
    }
}

/// Launch the matrix bridge runtime.
pub fn run(config: MatrixBridgeConfig) -> Result<(), String> {
    let project_name = crate::paths::detect_project_name()
        .ok_or_else(|| "Cannot determine project name for matrix bridge startup".to_string())?;
    let config = MatrixBridgeConfig {
        server_name: format!("{project_name}.local"),
        ..config
    };
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
        default_as_registration_config(&config, &project_name)?,
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

fn default_as_registration_config(
    config: &MatrixBridgeConfig,
    project_name: &str,
) -> Result<String, String> {
    let user_names = collect_coworker_identities(project_name);
    let channel_names = collect_channel_names(project_name);
    let user_pattern = regex_pattern(&user_names);
    let channel_pattern = regex_pattern(&channel_names);
    let as_token = random_token();
    let hs_token = random_token();
    let bridge_id = format!("midtown-bridge-{project_name}");

    Ok(format!(
        r##"id: {}
url: http://localhost:{}
as_token: {}
hs_token: {}
sender_localpart: {}
namespaces:
  users:
    - exclusive: true
      regex: "@({}):{}"
  rooms: []
  aliases:
    - exclusive: false
      regex: "#({}):{}"
"##,
        bridge_id,
        config.as_port,
        as_token,
        hs_token,
        project_name,
        user_pattern,
        config.server_name,
        channel_pattern,
        config.server_name
    ))
}

fn collect_coworker_identities(project_name: &str) -> Vec<String> {
    let mut identities = BTreeSet::new();

    if let Ok(state) = crate::daemon::state::DaemonPersistentState::load_for_repo(project_name) {
        for (name, info) in state.headless_sessions {
            if info.coworker_type.as_deref() == Some("channel-lead") {
                continue;
            }
            if crate::coworker::is_coworker_name(&name) {
                identities.insert(name);
            }
        }
        for session in state.sessions.into_values() {
            if session.coworker_type == "channel-lead" {
                continue;
            }
            if let Some(name) = session.current_name
                && crate::coworker::is_coworker_name(&name)
            {
                identities.insert(name);
            }
        }
    } else {
        identities.extend(
            crate::coworker::AVENUE_NAMES
                .iter()
                .map(|name| (*name).to_string()),
        );
    }

    if let Ok(entries) = std::fs::read_dir(crate::paths::coworkers_dir_for_repo(project_name)) {
        for entry in entries.flatten() {
            let is_dir = entry
                .file_type()
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !is_dir {
                continue;
            }
            if let Some(name) = entry.file_name().to_str()
                && crate::coworker::is_coworker_name(name)
            {
                identities.insert(name.to_string());
            }
        }
    }

    identities.insert(project_name.to_string());
    identities.into_iter().collect()
}

fn collect_channel_names(project_name: &str) -> Vec<String> {
    let mut names = BTreeSet::new();

    if let Ok(state) = crate::daemon::state::DaemonPersistentState::load_for_repo(project_name) {
        if state.channel_lead_sessions.is_empty() {
            names.insert(project_name.to_string());
            return names.into_iter().collect();
        }
        names.extend(
            state
                .channel_lead_sessions
                .keys()
                .filter(|name| is_channel_dir_name(name))
                .map(|name| name.trim_end_matches(".archived").to_string()),
        );
    }

    names.insert(project_name.to_string());
    names.into_iter().collect()
}

fn is_channel_dir_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.starts_with('.') || name == "notes" || name == "history" || name == "cursors" {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn regex_pattern(names: &[String]) -> String {
    let escaped: Vec<String> = names.iter().map(|name| regex::escape(name)).collect();
    if escaped.is_empty() {
        ".*".to_string()
    } else if escaped.len() == 1 {
        escaped[0].clone()
    } else {
        format!("(?:{})", escaped.join("|"))
    }
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
