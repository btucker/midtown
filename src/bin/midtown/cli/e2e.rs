use clap::Subcommand;

#[derive(Subcommand, Debug, Clone)]
pub enum E2eCommand {
    /// One-time auth setup: launch Claude with a dedicated config dir for OAuth login
    Auth,
    /// Run containerized E2E tests
    Run {
        /// Test mode to run
        #[arg(value_enum, default_value = "full")]
        mode: E2eMode,

        /// Extra arguments passed through to the test script
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Capture daemon WorldSnapshot for test fixtures
    ///
    /// Saves the full daemon WorldSnapshot (including all pane contents, coworker
    /// state, task state, etc.) to a JSON fixture file. Use this during normal
    /// operation to capture real daemon states for use in unit tests.
    Capture {
        /// Optional label to include in the filename (e.g., "usage-limit", "idle")
        #[arg(short, long)]
        label: Option<String>,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum E2eMode {
    /// Coordination-only tests (faster, no full stack)
    Coordination,
    /// Full stack E2E tests
    Full,
}

pub fn handle(cmd: &E2eCommand) -> Result<(), String> {
    match cmd {
        E2eCommand::Auth => handle_auth(),
        E2eCommand::Run { mode, args } => handle_run(mode, args),
        E2eCommand::Capture { label } => handle_capture(label.as_deref()),
    }
}

fn handle_auth() -> Result<(), String> {
    // Show deprecation warning
    eprintln!("⚠️  DEPRECATED: 'midtown e2e auth' is deprecated.");
    eprintln!("   Use 'midtown auth login --profile e2e' instead.");
    eprintln!();

    // Run migration if needed
    if let Ok(true) = midtown::auth::migrate_legacy_auth() {
        eprintln!("Note: Migrated existing auth to profile 'e2e'");
        eprintln!();
    }

    // Delegate to the new auth system with 'e2e' profile
    let auth_dir = midtown::auth::ensure_profile_dir("e2e")
        .map_err(|e| format!("Failed to create auth directory: {}", e))?;

    println!("Launching Claude with profile 'e2e'...");
    println!("Config dir: {}", auth_dir.display());
    println!();
    println!("Run /login inside the Claude session to authenticate.");
    println!("Once authenticated, exit the session. The tokens will be cached");
    println!("in {} for E2E test runs.", auth_dir.display());
    println!();

    let status = std::process::Command::new("claude")
        .env("CLAUDE_CONFIG_DIR", &auth_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("Failed to launch claude: {}. Is claude installed?", e))?;

    if status.success() {
        println!();
        println!("Auth setup complete. You can now run: midtown e2e run");
        println!();
        println!("Tip: Use 'midtown auth login --profile e2e' in the future.");
    } else {
        return Err(format!("Claude exited with status: {}", status));
    }

    Ok(())
}

fn handle_run(mode: &E2eMode, extra_args: &[String]) -> Result<(), String> {
    // Check Docker is available
    let docker_check = std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match docker_check {
        Ok(status) if status.success() => {}
        Ok(_) => {
            return Err("Docker is not running. Please start Docker and try again.".to_string());
        }
        Err(_) => {
            return Err(
                "Docker is not installed. Install Docker to run containerized E2E tests."
                    .to_string(),
            );
        }
    }

    // Locate the e2e container script
    let script = find_e2e_script()?;

    let mode_arg = match mode {
        E2eMode::Coordination => "coordination",
        E2eMode::Full => "full",
    };

    let mut cmd = std::process::Command::new(&script);
    cmd.arg(mode_arg);
    cmd.args(extra_args);

    // Inherit stdio so the user sees build/test output
    cmd.stdin(std::process::Stdio::inherit());
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    let status = cmd
        .status()
        .map_err(|e| format!("Failed to run '{}': {}", script.display(), e))?;

    if !status.success() {
        return Err(format!(
            "E2E tests failed with exit code: {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

/// Capture the full daemon WorldSnapshot and save to a JSON fixture file.
fn handle_capture(label: Option<&str>) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    // Connect to daemon socket
    let socket_path = midtown::paths::daemon_socket();
    let mut stream = UnixStream::connect(&socket_path).map_err(|e| {
        format!(
            "Could not connect to daemon socket: {}. Is the daemon running?",
            e
        )
    })?;

    // Make RPC call to get snapshot
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "snapshot",
        "id": 1
    });
    let request_line = format!("{}\n", request);
    stream
        .write_all(request_line.as_bytes())
        .map_err(|e| format!("Failed to send request: {}", e))?;
    stream
        .flush()
        .map_err(|e| format!("Failed to flush request: {}", e))?;

    // Read response
    let mut reader = BufReader::new(&stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .map_err(|e| format!("Failed to read response: {}", e))?;

    let response: serde_json::Value = serde_json::from_str(&response_line)
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Extract result
    let snapshot = response.get("result").ok_or_else(|| {
        let error = response.get("error");
        format!(
            "Daemon returned error: {}",
            error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into())
        )
    })?;

    // Create fixtures directory in the repo
    let fixtures_dir = find_fixtures_dir()?;
    std::fs::create_dir_all(&fixtures_dir)
        .map_err(|e| format!("Failed to create fixtures dir: {}", e))?;

    // Generate unique filename: snapshot-<label>-<timestamp>.json
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let filename = match label {
        Some(l) => format!("snapshot-{}-{}.json", l, timestamp),
        None => format!("snapshot-{}.json", timestamp),
    };
    let path = fixtures_dir.join(&filename);

    // Pretty-print the JSON
    let content = serde_json::to_string_pretty(snapshot)
        .map_err(|e| format!("Failed to serialize snapshot: {}", e))?;

    std::fs::write(&path, &content)
        .map_err(|e| format!("Failed to write fixture '{}': {}", path.display(), e))?;

    // Show summary
    let coworker_count = snapshot
        .get("active_coworkers")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let pane_count = snapshot
        .get("pane_contents")
        .and_then(|v| v.as_object())
        .map(|o| o.len())
        .unwrap_or(0);
    let task_count = snapshot
        .get("all_tasks")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let channel_message_count = snapshot
        .get("channel_messages")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let daemon_log_count = snapshot
        .get("daemon_logs")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    println!("Captured WorldSnapshot to: {}", path.display());
    println!();
    println!("Snapshot summary:");
    println!("  Active coworkers: {}", coworker_count);
    println!("  Pane contents: {}", pane_count);
    println!("  Tasks: {}", task_count);
    println!("  Channel messages: {}", channel_message_count);
    println!("  Daemon log lines: {}", daemon_log_count);
    println!("  File size: {} bytes", content.len());
    println!();
    println!("Use this fixture in tests by loading from:");
    println!("  tests/fixtures/snapshot/{}", filename);

    Ok(())
}

/// Find the fixtures directory by locating the repo root.
fn find_fixtures_dir() -> Result<std::path::PathBuf, String> {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            // Look for Cargo.toml as marker for repo root
            if dir.join("Cargo.toml").exists() {
                return Ok(dir.join("tests/fixtures/snapshot"));
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }
    Err(
        "Could not find repository root (Cargo.toml). Are you in the midtown repository?"
            .to_string(),
    )
}

/// Find the e2e-container.sh script by searching upward from the current dir
/// or checking the cargo manifest dir.
fn find_e2e_script() -> Result<std::path::PathBuf, String> {
    // Try relative to the repo root (walk up from cwd looking for scripts/e2e-container.sh)
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join("scripts/e2e-container.sh");
            if candidate.exists() {
                return Ok(candidate);
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }

    Err("Could not find scripts/e2e-container.sh. Are you in the midtown repository?".to_string())
}
