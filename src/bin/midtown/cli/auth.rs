//! CLI handlers for auth profile management.

use clap::Subcommand;

use super::Response;

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommand {
    /// Log in to a profile (creates if needed, launches Claude for OAuth)
    Login {
        /// Profile name (default: "default")
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// List all profiles
    List,
    /// Switch to a different profile
    Switch {
        /// Profile name to switch to
        profile: String,
    },
    /// Show current profile status
    Status,
    /// Remove a profile
    Remove {
        /// Profile name to remove
        profile: String,
    },
}

pub fn handle(cmd: &AuthCommand) -> Result<Response, String> {
    // Run legacy auth migration on any auth command
    if let Ok(true) = midtown::auth::migrate_legacy_auth() {
        eprintln!(
            "Note: Migrated legacy auth directory (~/.midtown/claude-auth/) to profile 'e2e'"
        );
    }

    match cmd {
        AuthCommand::Login { profile } => handle_login(profile),
        AuthCommand::List => handle_list(),
        AuthCommand::Switch { profile } => handle_switch(profile),
        AuthCommand::Status => handle_status(),
        AuthCommand::Remove { profile } => handle_remove(profile),
    }
}

/// Format a reset time as a relative duration (e.g., "2h 15m")
fn format_reset_time(resets: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = resets.signed_duration_since(now);
    let hours = duration.num_hours();
    let minutes = duration.num_minutes() % 60;
    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}

fn handle_login(profile: &str) -> Result<Response, String> {
    let profile_dir = midtown::auth::ensure_profile_dir(profile)
        .map_err(|e| format!("Failed to create profile directory: {}", e))?;

    println!("Launching Claude with profile '{}'...", profile);
    println!("Config dir: {}", profile_dir.display());
    println!();
    println!("Run /login inside the Claude session to authenticate.");
    println!("Once authenticated, exit the session. The tokens will be cached");
    println!("in {} for future use.", profile_dir.display());
    println!();

    let status = std::process::Command::new("claude")
        .env("CLAUDE_CONFIG_DIR", &profile_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("Failed to launch claude: {}. Is claude installed?", e))?;

    if !status.success() {
        return Err(format!("Claude exited with status: {}", status));
    }

    // If this is the first profile, set it as current
    let profiles = midtown::auth::list_profiles().unwrap_or_default();
    if profiles.len() == 1
        && let Err(e) = midtown::auth::set_current_profile(profile)
    {
        eprintln!(
            "Warning: Could not set '{}' as current profile: {}",
            profile, e
        );
    }

    Ok(Response::Message {
        message: format!("Profile '{}' authenticated successfully.", profile),
    })
}

fn handle_list() -> Result<Response, String> {
    let profiles =
        midtown::auth::list_profiles().map_err(|e| format!("Failed to list profiles: {}", e))?;

    if profiles.is_empty() {
        return Ok(Response::Message {
            message: "No profiles found. Create one with: midtown auth login".to_string(),
        });
    }

    let current = midtown::auth::current_profile();

    // Fetch usage data for all authenticated profiles in parallel
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;
    let usage_results: Vec<(String, Option<midtown::UsageData>)> = runtime.block_on(async {
        let futures = profiles.iter().map(|name| {
            let name = name.clone();
            tokio::task::spawn_blocking(move || {
                let usage = midtown::fetch_usage_for_profile(&name);
                (name, usage)
            })
        });

        let results = futures::future::join_all(futures).await;
        results.into_iter().filter_map(|r| r.ok()).collect()
    });

    // Build table rows
    let mut rows = Vec::new();

    for name in &profiles {
        let marker = if *name == current { " (active)" } else { "" };
        let status = midtown::auth::profile_status(name);

        let (email, cred_status, session_str, session_reset_str, weekly_str, weekly_reset_str) =
            match status {
                Some(s) if s.has_credentials => {
                    // Find usage data for this profile
                    let usage = usage_results
                        .iter()
                        .find(|(n, _)| n == name)
                        .and_then(|(_, u)| u.as_ref());

                    match usage {
                        Some(data) => {
                            let email_display = data
                                .account_email
                                .as_ref()
                                .map(|e| format!("{}{}", e, marker))
                                .unwrap_or_else(|| format!("{}{}", name, marker));

                            // Format session (5-hour) usage
                            let session_display = format!("{:.0}%", data.session_util);
                            let session_reset_display = format_reset_time(data.session_resets);

                            // Format weekly (7-day) usage
                            let weekly_display = format!("{:.0}%", data.week_util);
                            let weekly_reset_display = format_reset_time(data.week_resets);

                            (
                                email_display,
                                "authenticated".to_string(),
                                session_display,
                                session_reset_display,
                                weekly_display,
                                weekly_reset_display,
                            )
                        }
                        None => (
                            format!("{}{}", name, marker),
                            "authenticated".to_string(),
                            "unavailable".to_string(),
                            "-".to_string(),
                            "unavailable".to_string(),
                            "-".to_string(),
                        ),
                    }
                }
                Some(_) => (
                    format!("{}{}", name, marker),
                    "not authenticated".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                ),
                None => (
                    format!("{}{}", name, marker),
                    "unknown".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                ),
            };

        rows.push((
            email,
            cred_status,
            session_str,
            session_reset_str,
            weekly_str,
            weekly_reset_str,
        ));
    }

    // Calculate column widths
    let max_email = rows
        .iter()
        .map(|(e, _, _, _, _, _)| e.len())
        .max()
        .unwrap_or(0)
        .max("Account".len());
    let max_status = rows
        .iter()
        .map(|(_, s, _, _, _, _)| s.len())
        .max()
        .unwrap_or(0)
        .max("Status".len());
    let max_session = rows
        .iter()
        .map(|(_, _, ss, _, _, _)| ss.len())
        .max()
        .unwrap_or(0)
        .max("Session".len());
    let max_session_reset = rows
        .iter()
        .map(|(_, _, _, sr, _, _)| sr.len())
        .max()
        .unwrap_or(0)
        .max("Resets".len());
    let max_weekly = rows
        .iter()
        .map(|(_, _, _, _, ws, _)| ws.len())
        .max()
        .unwrap_or(0)
        .max("Weekly".len());
    let max_weekly_reset = rows
        .iter()
        .map(|(_, _, _, _, _, wr)| wr.len())
        .max()
        .unwrap_or(0)
        .max("Resets".len());

    // Format table
    let mut lines = Vec::new();
    lines.push(format!(
        "{:<width_e$}  {:<width_s$}  {:<width_ss$}  {:<width_sr$}  {:<width_ws$}  {:<width_wr$}",
        "Account",
        "Status",
        "Session",
        "Resets",
        "Weekly",
        "Resets",
        width_e = max_email,
        width_s = max_status,
        width_ss = max_session,
        width_sr = max_session_reset,
        width_ws = max_weekly,
        width_wr = max_weekly_reset,
    ));

    lines.push(format!(
        "{}  {}  {}  {}  {}  {}",
        "─".repeat(max_email),
        "─".repeat(max_status),
        "─".repeat(max_session),
        "─".repeat(max_session_reset),
        "─".repeat(max_weekly),
        "─".repeat(max_weekly_reset),
    ));

    for (email, status, session, session_reset, weekly, weekly_reset) in rows {
        lines.push(format!(
            "{:<width_e$}  {:<width_s$}  {:<width_ss$}  {:<width_sr$}  {:<width_ws$}  {:<width_wr$}",
            email,
            status,
            session,
            session_reset,
            weekly,
            weekly_reset,
            width_e = max_email,
            width_s = max_status,
            width_ss = max_session,
            width_sr = max_session_reset,
            width_ws = max_weekly,
            width_wr = max_weekly_reset,
        ));
    }

    Ok(Response::Message {
        message: lines.join("\n"),
    })
}

fn handle_switch(profile: &str) -> Result<Response, String> {
    // If the daemon is running, use RPC to switch profile and re-launch all claudes.
    // This ensures running coworkers and the lead pick up the new credentials.
    if let Ok(client) = crate::client::DaemonClient::connect() {
        return client.auth_switch(profile);
    }

    // No daemon running — just switch the profile file for the next session.
    midtown::auth::set_current_profile(profile).map_err(|e| e.to_string())?;

    Ok(Response::Message {
        message: format!(
            "Switched to profile '{}'. No daemon running — new sessions will use this profile.",
            profile
        ),
    })
}

fn handle_status() -> Result<Response, String> {
    let current = midtown::auth::current_profile();
    let status = midtown::auth::profile_status(&current);

    match status {
        Some(s) => {
            let cred_status = if s.has_credentials {
                "authenticated"
            } else {
                "not authenticated (run: midtown auth login)"
            };
            let message = format!(
                "Current profile: {}\nConfig dir: {}\nStatus: {}",
                s.name,
                s.path.display(),
                cred_status
            );
            Ok(Response::Message { message })
        }
        None => {
            let dir = midtown::auth::profile_dir(&current);
            Ok(Response::Message {
                message: format!(
                    "Current profile: {} (not initialized)\nConfig dir: {}\nRun 'midtown auth login' to set up authentication.",
                    current,
                    dir.display()
                ),
            })
        }
    }
}

fn handle_remove(profile: &str) -> Result<Response, String> {
    midtown::auth::remove_profile(profile).map_err(|e| e.to_string())?;

    Ok(Response::Message {
        message: format!("Removed profile '{}'.", profile),
    })
}
