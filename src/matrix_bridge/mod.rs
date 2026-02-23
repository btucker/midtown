use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

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

    let access_token =
        std::env::var("MIDTOWN_MATRIX_ACCESS_TOKEN").unwrap_or_else(|_| "matrix".to_string());
    let matrix_client = client::MatrixClient::new(
        format!("http://127.0.0.1:{}", config.matrix_port),
        access_token,
    );

    let mut state = MatrixBridgeState::load(&state_path)?;
    let mut state_updated = false;
    state_updated |= sync_identity_users(&matrix_client, &project_name, &mut state)?;
    state_updated |= sync_channel_rooms(&matrix_client, &project_name, &mut state)?;
    if state_updated {
        state.save(&state_path)?;
    }

    let outbound_project_name = project_name.clone();
    let outbound_state_path = state_path.clone();
    let outbound_client = matrix_client.clone();
    thread::Builder::new()
        .name("matrix-outbound-sync".to_string())
        .spawn(move || {
            sync::run_outbound_sync(
                &outbound_client,
                &outbound_project_name,
                &outbound_state_path,
            )
            .map(|_| ())
            .unwrap_or_else(|e| {
                eprintln!("matrix outbound sync exited unexpectedly: {e}");
            });
        })
        .map_err(|e| format!("Failed to spawn outbound sync thread: {e}"))?;

    as_server::run_as_server(
        config.as_port,
        &project_name,
        matrix_client.homeserver_domain(),
        &state_path,
    )
}

fn sync_identity_users(
    client: &client::MatrixClient,
    project_name: &str,
    state: &mut MatrixBridgeState,
) -> Result<bool, String> {
    let identities = collect_coworker_identities(project_name);
    let mut updated = false;

    for identity in identities {
        if state.users.contains_key(&identity) {
            continue;
        }

        client.ensure_virtual_user_exists(&identity)?;
        state
            .users
            .insert(identity.clone(), client.user_id(&identity));
        updated = true;
    }

    Ok(updated)
}

fn sync_channel_rooms(
    client: &client::MatrixClient,
    project_name: &str,
    state: &mut MatrixBridgeState,
) -> Result<bool, String> {
    let channel_names = collect_channel_names(project_name);
    let mut updated = false;

    for channel_name in channel_names {
        if state.rooms.contains_key(&channel_name) {
            continue;
        }

        let room_id = client.ensure_room_exists(&channel_name)?;
        state.rooms.insert(channel_name, room_id);
        updated = true;
    }

    Ok(updated)
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
    let bridge_id = format!("midtown-{project_name}");

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
