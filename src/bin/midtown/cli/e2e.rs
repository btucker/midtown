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
    }
}

fn handle_auth() -> Result<(), String> {
    let auth_dir = midtown::paths::midtown_base_dir().join("claude-auth");

    std::fs::create_dir_all(&auth_dir).map_err(|e| {
        format!(
            "Failed to create auth directory '{}': {}",
            auth_dir.display(),
            e
        )
    })?;

    println!("Launching Claude with dedicated auth config...");
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
