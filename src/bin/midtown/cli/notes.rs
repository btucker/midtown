//! CLI handlers for `midtown notes` subcommands.

use clap::Subcommand;

use super::Response;

#[derive(Subcommand, Debug, Clone)]
pub enum NotesCommand {
    /// Mark a note as reviewed (stamps reviewed_at to now)
    Review {
        /// Path to the note file (absolute or relative)
        path: String,
    },
    /// List notes, optionally filtered by channel or staleness
    List {
        /// Filter to a specific channel
        #[arg(long)]
        channel: Option<String>,
        /// Only show stale notes (not reviewed within threshold)
        #[arg(long)]
        stale: bool,
    },
}

/// Handle notes subcommands (no daemon required — operates on local files).
pub fn handle(cmd: &NotesCommand) -> Result<Response, String> {
    match cmd {
        NotesCommand::Review { path } => handle_review(path),
        NotesCommand::List { channel, stale } => handle_list(channel.as_deref(), *stale),
    }
}

fn handle_review(path: &str) -> Result<Response, String> {
    let path = std::path::Path::new(path);
    if !path.exists() {
        return Err(format!("Note file not found: {}", path.display()));
    }
    if path.extension().and_then(|e| e.to_str()) != Some("md") {
        return Err("Note file must have .md extension".to_string());
    }

    midtown::channel::stamp_note_reviewed(path)
        .map_err(|e| format!("Failed to stamp note: {}", e))?;

    let filename = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    Ok(Response::Message {
        message: format!("Marked '{}' as reviewed", filename),
    })
}

fn handle_list(channel: Option<&str>, stale_only: bool) -> Result<Response, String> {
    let repo_name =
        midtown::paths::detect_repo_name().ok_or_else(|| "Not in a git repo".to_string())?;
    let base_dir = midtown::paths::projects_dir_for_repo(&repo_name);

    let now = chrono::Utc::now();
    let threshold = chrono::Duration::hours(midtown::channel::NOTE_STALENESS_THRESHOLD_HOURS);

    let channels_to_check: Vec<String> = if let Some(ch) = channel {
        vec![ch.to_string()]
    } else {
        midtown::channel::Channel::list(&base_dir, false, None)
            .map_err(|e| format!("Failed to list channels: {}", e))?
            .into_iter()
            .filter(|c| !c.is_archived && !c.is_dm)
            .map(|c| c.name)
            .collect()
    };

    let mut lines = Vec::new();
    let mut total_count = 0;

    for ch_name in &channels_to_check {
        let notes = midtown::channel::list_channel_note_infos(&base_dir, ch_name);
        if notes.is_empty() {
            continue;
        }

        let filtered: Vec<_> = if stale_only {
            notes
                .into_iter()
                .filter(|n| match n.reviewed_at {
                    None => true,
                    Some(reviewed) => now - reviewed > threshold,
                })
                .collect()
        } else {
            notes
        };

        if filtered.is_empty() {
            continue;
        }

        lines.push(format!("## {}", ch_name));
        for note in &filtered {
            let status = match note.reviewed_at {
                None => "never reviewed".to_string(),
                Some(reviewed) => {
                    let age = now - reviewed;
                    if age > threshold {
                        format!("stale (reviewed {}d ago)", age.num_days())
                    } else {
                        format!("reviewed {}d ago", age.num_days())
                    }
                }
            };
            lines.push(format!("  {} — {}", note.name, status));
            lines.push(format!("    {}", note.path.display()));
        }
        total_count += filtered.len();
        lines.push(String::new());
    }

    if total_count == 0 {
        let msg = if stale_only {
            "No stale notes found"
        } else {
            "No notes found"
        };
        return Ok(Response::Message {
            message: msg.to_string(),
        });
    }

    Ok(Response::Message {
        message: lines.join("\n"),
    })
}
