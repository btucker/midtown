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

/// Profile info with usage data for display.
type ProfileInfo = (String, bool, bool, Option<midtown::usage::UsageData>);

fn handle_list() -> Result<Response, String> {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let profiles =
        midtown::auth::list_profiles().map_err(|e| format!("Failed to list profiles: {}", e))?;

    if profiles.is_empty() {
        return Ok(Response::Message {
            message: "No profiles found. Create one with: midtown auth login".to_string(),
        });
    }

    let current = midtown::auth::current_profile();

    // Fetch usage data for all authenticated profiles in parallel
    let results: Arc<Mutex<Vec<ProfileInfo>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = vec![];

    for name in profiles {
        let results = Arc::clone(&results);
        let name = name.clone();
        let is_current = name == current;

        let handle = thread::spawn(move || {
            let status = midtown::auth::profile_status(&name);
            let has_credentials = status.as_ref().is_some_and(|s| s.has_credentials);

            let usage = if has_credentials {
                midtown::usage::fetch_usage_for_profile(&name)
            } else {
                None
            };

            results
                .lock()
                .unwrap()
                .push((name, is_current, has_credentials, usage));
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    let mut results = results.lock().unwrap().clone();
    results.sort_by(|a, b| a.0.cmp(&b.0));

    // Format as a table
    let mut lines = Vec::new();
    lines.push(format!(
        "{:<20} {:<18} {:<25} {}",
        "Profile", "Status", "Usage", "Resets"
    ));
    lines.push("-".repeat(90));

    for (name, is_current, has_credentials, usage) in results {
        let marker = if is_current { " (active)" } else { "" };
        let profile_name = format!("{}{}", name, marker);

        let status = if has_credentials {
            "authenticated"
        } else {
            "not authenticated"
        };

        let (usage_str, resets_str) = if let Some(usage_data) = usage {
            // Use session (5-hour) utilization as the primary metric
            let pct = usage_data.session_util;
            let usage_display = format!("{:.0}%", pct);

            // Calculate time until reset
            let now = chrono::Utc::now();
            let duration = usage_data.session_resets.signed_duration_since(now);
            let hours = duration.num_hours();
            let minutes = duration.num_minutes() % 60;
            let resets_display = if hours > 0 {
                format!("{}h {}m", hours, minutes)
            } else {
                format!("{}m", minutes)
            };

            (usage_display, resets_display)
        } else if has_credentials {
            ("unavailable".to_string(), "-".to_string())
        } else {
            ("-".to_string(), "-".to_string())
        };

        lines.push(format!(
            "{:<20} {:<18} {:<25} {}",
            profile_name, status, usage_str, resets_str
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
