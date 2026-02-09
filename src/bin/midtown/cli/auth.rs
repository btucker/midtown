//! CLI handlers for auth profile management.

use std::io::IsTerminal;

use clap::Subcommand;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal, TerminalOptions, Viewport,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
};

use super::Response;

#[derive(Subcommand, Debug, Clone)]
pub enum AuthCommand {
    /// Log in to a profile (creates if needed, launches Claude for OAuth)
    Login {
        /// Email address for the profile (e.g., user@example.com)
        email: String,
    },
    /// List all profiles and interactively switch
    List,
    /// Switch to a different profile
    Switch {
        /// Profile name to switch to
        profile: String,

        /// Switch all projects (not just the current one)
        #[arg(long)]
        all: bool,
    },
    /// Remove a profile
    Remove {
        /// Profile name to remove
        profile: String,
    },
}

pub fn handle(cmd: &AuthCommand) -> Result<Response, String> {
    match cmd {
        AuthCommand::Login { email } => handle_login(email),
        AuthCommand::List => handle_list(),
        AuthCommand::Switch { profile, all } => handle_switch(profile, *all),
        AuthCommand::Remove { profile } => handle_remove(profile),
    }
}

fn handle_login(email: &str) -> Result<Response, String> {
    // Validate email format (must contain @)
    if !email.contains('@') {
        return Err(format!(
            "Invalid email '{}'. Use an email address (e.g., user@example.com).",
            email
        ));
    }

    let profile_dir = midtown::auth::ensure_profile_dir(email)
        .map_err(|e| format!("Failed to create profile directory: {}", e))?;

    println!("Launching Claude with profile '{}'...", email);
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
        && let Err(e) = midtown::auth::set_current_profile(email)
    {
        eprintln!(
            "Warning: Could not set '{}' as current profile: {}",
            email, e
        );
    }

    Ok(Response::Message {
        message: format!("Profile '{}' authenticated successfully.", email),
    })
}

/// Data for a profile row in the interactive list.
struct ProfileRow {
    name: String,
    is_current: bool,
    has_credentials: bool,
    usage: Option<midtown::UsageData>,
    /// Remaining capacity = min(100 - session_util, 100 - week_util).
    /// Higher is better. None for profiles without usage data.
    remaining_capacity: Option<f64>,
    /// Soonest bottleneck reset time (for tiebreaking at 0% remaining).
    bottleneck_reset: Option<chrono::DateTime<chrono::Utc>>,
}

/// Fetch profiles with usage data, sorted by available capacity (best first).
fn fetch_sorted_profiles() -> Result<Vec<ProfileRow>, String> {
    let profiles =
        midtown::auth::list_profiles().map_err(|e| format!("Failed to list profiles: {}", e))?;

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

    let mut rows: Vec<ProfileRow> = profiles
        .iter()
        .map(|name| {
            let status = midtown::auth::profile_status(name);
            let has_credentials = status.as_ref().is_some_and(|s| s.has_credentials);
            let usage = usage_results
                .iter()
                .find(|(n, _)| n == name)
                .and_then(|(_, u)| u.clone());

            let (remaining_capacity, bottleneck_reset) = if let Some(ref data) = usage {
                let session_remaining = 100.0 - data.session_util;
                let week_remaining = 100.0 - data.week_util;
                let remaining = session_remaining.min(week_remaining);
                // Bottleneck reset = the reset of whichever limit is tighter
                let reset = if session_remaining <= week_remaining {
                    data.session_resets
                } else {
                    data.week_resets
                };
                (Some(remaining), reset)
            } else {
                (None, None)
            };

            ProfileRow {
                name: name.clone(),
                is_current: *name == current,
                has_credentials,
                usage,
                remaining_capacity,
                bottleneck_reset,
            }
        })
        .collect();

    // Sort by remaining capacity (highest first), then by soonest bottleneck reset.
    // Profiles without usage data sort to bottom.
    rows.sort_by(|a, b| {
        match (a.remaining_capacity, b.remaining_capacity) {
            (Some(a_cap), Some(b_cap)) => {
                // Higher remaining capacity is better (sort descending)
                b_cap
                    .partial_cmp(&a_cap)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        // Tiebreaker: soonest bottleneck reset (for accounts at 0%)
                        match (a.bottleneck_reset, b.bottleneck_reset) {
                            (Some(a_reset), Some(b_reset)) => a_reset.cmp(&b_reset),
                            _ => std::cmp::Ordering::Equal,
                        }
                    })
            }
            (Some(_), None) => std::cmp::Ordering::Less, // a has data, b doesn't
            (None, Some(_)) => std::cmp::Ordering::Greater, // b has data, a doesn't
            (None, None) => std::cmp::Ordering::Equal,
        }
    });

    Ok(rows)
}

/// Format a reset time: relative ("2h 15m") if under 24h, absolute ("Feb 11 @ 10:59am") otherwise.
fn format_relative_time(target: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let duration = target.signed_duration_since(now);
    let hours = duration.num_hours();
    if hours >= 24 {
        let local = target.with_timezone(&chrono::Local);
        local.format("%a %b %-d @ %-I:%M%P").to_string()
    } else if hours > 0 {
        let minutes = duration.num_minutes() % 60;
        format!("{}h {}m", hours, minutes)
    } else {
        let minutes = duration.num_minutes() % 60;
        format!("{}m", minutes)
    }
}

/// Detailed usage info for a profile: session and weekly limits + resets.
struct UsageDisplay {
    session_util: String,
    session_resets: String,
    week_util: String,
    week_resets: String,
}

/// Format detailed usage display for a profile row.
fn format_usage_detail(row: &ProfileRow) -> Option<UsageDisplay> {
    let data = row.usage.as_ref()?;
    Some(UsageDisplay {
        session_util: format!("{:.0}%", data.session_util),
        session_resets: data
            .session_resets
            .map(format_relative_time)
            .unwrap_or_else(|| "-".to_string()),
        week_util: format!("{:.0}%", data.week_util),
        week_resets: data
            .week_resets
            .map(format_relative_time)
            .unwrap_or_else(|| "-".to_string()),
    })
}

/// Result of the interactive selector.
enum SelectorAction {
    /// User selected a profile to switch to.
    Switch(String),
    /// User wants to add a new account.
    AddAccount,
    /// User wants to remove a profile.
    Remove(String),
    /// User cancelled.
    Cancel,
}

fn handle_list() -> Result<Response, String> {
    let rows = fetch_sorted_profiles()?;

    // Non-TTY: static table only
    if !std::io::stdout().is_terminal() {
        if rows.is_empty() {
            return Ok(Response::Message {
                message: "No profiles found. Create one with: midtown auth login <email>"
                    .to_string(),
            });
        }
        return Ok(Response::Message {
            message: format_table(&rows),
        });
    }

    // Print the table first so usage details are visible above the selector
    if !rows.is_empty() {
        println!("{}", format_table(&rows));
        println!();
    }

    // Interactive selector below the table
    let action = run_interactive_selector(&rows)?;

    match action {
        SelectorAction::Switch(profile) => {
            let all = run_scope_selector()?;
            match all {
                Some(scope_all) => handle_switch(&profile, scope_all),
                None => Ok(Response::Message {
                    message: String::new(),
                }), // cancelled
            }
        }
        SelectorAction::AddAccount => prompt_add_account(),
        SelectorAction::Remove(profile) => confirm_and_remove(&profile),
        SelectorAction::Cancel => Ok(Response::Message {
            message: String::new(),
        }),
    }
}

/// Confirm and remove a profile.
fn confirm_and_remove(profile: &str) -> Result<Response, String> {
    eprint!("Remove profile '{}'? [y/N] ", profile);
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {}", e))?;
    if input.trim().eq_ignore_ascii_case("y") {
        handle_remove(profile)
    } else {
        Ok(Response::Message {
            message: "Cancelled.".to_string(),
        })
    }
}

/// Prompt the user for an email and run the login flow.
fn prompt_add_account() -> Result<Response, String> {
    eprint!("Email: ");
    let mut email = String::new();
    std::io::stdin()
        .read_line(&mut email)
        .map_err(|e| format!("Failed to read input: {}", e))?;
    let email = email.trim();
    if email.is_empty() {
        return Ok(Response::Message {
            message: "Cancelled.".to_string(),
        });
    }
    handle_login(email)
}

/// Format profiles as a static table string.
fn format_table(rows: &[ProfileRow]) -> String {
    let mut lines = Vec::new();

    // Build display rows: (profile, session_usage, session_resets, week_usage, week_resets)
    let display_rows: Vec<(String, String, String, String, String)> = rows
        .iter()
        .map(|row| {
            let marker = if row.is_current { " *" } else { "" };
            let profile = format!("{}{}", row.name, marker);
            if !row.has_credentials {
                return (
                    profile,
                    "no auth".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                );
            }
            match format_usage_detail(row) {
                Some(d) => (
                    profile,
                    d.session_util,
                    d.session_resets,
                    d.week_util,
                    d.week_resets,
                ),
                None => (profile, "-".into(), "-".into(), "-".into(), "-".into()),
            }
        })
        .collect();

    let w = |col: usize, header: &str| -> usize {
        display_rows
            .iter()
            .map(|r| match col {
                0 => r.0.len(),
                1 => r.1.len(),
                2 => r.2.len(),
                3 => r.3.len(),
                _ => r.4.len(),
            })
            .max()
            .unwrap_or(0)
            .max(header.len())
    };

    let wp = w(0, "Profile");
    let ws = w(1, "Session");
    let wsr = w(2, "Resets");
    let ww = w(3, "Week");
    let wwr = w(4, "Resets");

    lines.push(format!(
        "{:<wp$}  {:<ws$}  {:<wsr$}  {:<ww$}  {:<wwr$}",
        "Profile", "Session", "Resets", "Week", "Resets",
    ));
    let sep = "\u{2500}";
    lines.push(format!(
        "{}  {}  {}  {}  {}",
        sep.repeat(wp),
        sep.repeat(ws),
        sep.repeat(wsr),
        sep.repeat(ww),
        sep.repeat(wwr),
    ));
    for (profile, su, sr, wu, wr) in &display_rows {
        lines.push(format!(
            "{:<wp$}  {:<ws$}  {:<wsr$}  {:<ww$}  {:<wwr$}",
            profile, su, sr, wu, wr,
        ));
    }

    lines.join("\n")
}

/// Run the interactive TUI selector inline below the cursor.
fn run_interactive_selector(rows: &[ProfileRow]) -> Result<SelectorAction, String> {
    // Find current profile's index for pre-selection (or 0 for the "+ Add account" row)
    let current_idx = rows.iter().position(|r| r.is_current).unwrap_or(0);
    let mut state = ListState::default();
    state.select(Some(current_idx));

    // +3 = 1 for "Add account" row + 2 for borders
    let viewport_height = rows.len() as u16 + 3;

    enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {}", e))?;
    let stdout = std::io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(viewport_height),
        },
    )
    .map_err(|e| format!("Failed to create terminal: {}", e))?;

    let result = run_selector_loop(&mut terminal, rows, &mut state);

    // Move cursor below the inline viewport before restoring
    let pos = terminal
        .get_cursor_position()
        .map_err(|e| format!("Failed to get cursor: {}", e))?;
    terminal
        .set_cursor_position(ratatui::layout::Position::new(0, pos.y + viewport_height))
        .map_err(|e| format!("Failed to set cursor: {}", e))?;

    // Restore terminal
    disable_raw_mode().map_err(|e| format!("Failed to disable raw mode: {}", e))?;

    result
}

/// Inline selector for switch scope: this project (default) or all projects.
/// Returns Some(true) for all, Some(false) for this project, None for cancel.
fn run_scope_selector() -> Result<Option<bool>, String> {
    let options = ["This project (default)", "All projects"];
    let mut state = ListState::default();
    state.select(Some(0));

    // +2 for borders
    let viewport_height = options.len() as u16 + 2;

    enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {}", e))?;
    let stdout = std::io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(viewport_height),
        },
    )
    .map_err(|e| format!("Failed to create terminal: {}", e))?;

    let result = loop {
        terminal
            .draw(|f| {
                let title = " Switch scope (Enter=confirm, Esc=cancel) ";
                let items: Vec<ListItem> = options
                    .iter()
                    .map(|o| ListItem::new(Line::from(*o)))
                    .collect();
                let max_width = options.iter().map(|o| o.len()).max().unwrap_or(0);
                let width = (max_width as u16 + 4)
                    .max(title.len() as u16 + 2)
                    .min(f.area().width);
                let list = List::new(items)
                    .block(
                        Block::default()
                            .title(title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::DarkGray)),
                    )
                    .highlight_style(
                        Style::default()
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    );
                let full = f.area();
                let area = Rect::new(full.x, full.y, width, full.height);
                f.render_stateful_widget(list, area, &mut state);
            })
            .map_err(|e| format!("Draw error: {}", e))?;

        if let Ok(Event::Key(key)) = event::read() {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(None),
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some(if i == 0 { options.len() - 1 } else { i - 1 }));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some((i + 1) % options.len()));
                }
                KeyCode::Enter => {
                    let all = state.selected() == Some(1);
                    break Ok(Some(all));
                }
                _ => {}
            }
        }
    };

    // Move cursor below the inline viewport before restoring
    let pos = terminal
        .get_cursor_position()
        .map_err(|e| format!("Failed to get cursor: {}", e))?;
    terminal
        .set_cursor_position(ratatui::layout::Position::new(0, pos.y + viewport_height))
        .map_err(|e| format!("Failed to set cursor: {}", e))?;

    disable_raw_mode().map_err(|e| format!("Failed to disable raw mode: {}", e))?;

    result
}

fn run_selector_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    rows: &[ProfileRow],
    state: &mut ListState,
) -> Result<SelectorAction, String> {
    // Total items = profiles + 1 "Add account" entry at the end
    let total = rows.len() + 1;
    let add_account_idx = rows.len();

    loop {
        terminal
            .draw(|f| draw_selector(f, rows, state))
            .map_err(|e| format!("Draw error: {}", e))?;

        if let Ok(Event::Key(key)) = event::read() {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(SelectorAction::Cancel),
                KeyCode::Up | KeyCode::Char('k') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some(if i == 0 { total - 1 } else { i - 1 }));
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let i = state.selected().unwrap_or(0);
                    state.select(Some((i + 1) % total));
                }
                KeyCode::Enter => {
                    if let Some(i) = state.selected() {
                        if i == add_account_idx {
                            return Ok(SelectorAction::AddAccount);
                        }
                        let row = &rows[i];
                        if row.is_current {
                            return Ok(SelectorAction::Cancel);
                        }
                        return Ok(SelectorAction::Switch(row.name.clone()));
                    }
                }
                KeyCode::Backspace | KeyCode::Delete => {
                    if let Some(i) = state.selected()
                        && i < rows.len()
                    {
                        return Ok(SelectorAction::Remove(rows[i].name.clone()));
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw_selector(f: &mut ratatui::Frame, rows: &[ProfileRow], state: &mut ListState) {
    let mut items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let indicator = if row.is_current {
                "\u{25cf}"
            } else {
                "\u{25cb}"
            };

            let style = if row.is_current {
                Style::default().fg(Color::Cyan)
            } else if !row.has_credentials {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };

            let suffix = if row.is_current {
                " (active)"
            } else if !row.has_credentials {
                " (no auth)"
            } else {
                ""
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", indicator), style),
                Span::styled(row.name.clone(), style.add_modifier(Modifier::BOLD)),
                Span::styled(suffix, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    // "Add account" entry at the bottom
    let green = Style::default().fg(Color::Green);
    items.push(ListItem::new(Line::from(vec![
        Span::styled("+ ", green),
        Span::styled("Add account", green),
    ])));

    let title = " Enter=switch  Del=remove  Esc=cancel ";

    // Compute width: widest row content + 2 for borders + 2 for highlight padding
    let max_row_width = rows
        .iter()
        .map(|row| {
            let suffix_len = if row.is_current {
                " (active)".len()
            } else if !row.has_credentials {
                " (no auth)".len()
            } else {
                0
            };
            2 + row.name.len() + suffix_len // "● " prefix + name + suffix
        })
        .max()
        .unwrap_or(0)
        .max("+ Add account".len());
    let width = (max_row_width as u16 + 4) // +2 borders, +2 padding
        .max(title.len() as u16 + 2) // title must fit within borders
        .min(f.area().width);

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    let full = f.area();
    let area = Rect::new(full.x, full.y, width, full.height);
    f.render_stateful_widget(list, area, state);
}

fn handle_switch(profile: &str, all: bool) -> Result<Response, String> {
    // If the daemon is running, use RPC to switch profile and re-launch all claudes.
    // This ensures running coworkers and the lead pick up the new credentials.
    if let Ok(client) = crate::client::DaemonClient::connect() {
        return client.auth_switch(profile, all);
    }

    if all {
        // Global switch: update the global current profile
        midtown::auth::set_current_profile(profile).map_err(|e| e.to_string())?;

        Ok(Response::Message {
            message: format!(
                "Switched all projects to profile '{}'. No daemon running — new sessions will use this profile.",
                profile
            ),
        })
    } else {
        // Per-project switch: update current project's config
        let project_name = midtown::paths::detect_repo_name()
            .ok_or_else(|| "Not in a git repository. Use --all to switch globally.".to_string())?;

        set_project_auth_profile(&project_name, profile)?;

        Ok(Response::Message {
            message: format!(
                "Switched project '{}' to profile '{}'. No daemon running — new sessions will use this profile.",
                project_name, profile
            ),
        })
    }
}

/// Set the auth_profile in a project's config.toml.
fn set_project_auth_profile(project_name: &str, profile: &str) -> Result<(), String> {
    let path = midtown::config::project_config_path(project_name);
    let mut config = midtown::config::FullProjectConfig::load_from(&path).unwrap_or_default();
    config.project.auth_profile = Some(profile.to_string());
    config
        .save_to(&path)
        .map_err(|e| format!("Failed to save project config: {}", e))
}

fn handle_remove(profile: &str) -> Result<Response, String> {
    midtown::auth::remove_profile(profile).map_err(|e| e.to_string())?;

    Ok(Response::Message {
        message: format!("Removed profile '{}'.", profile),
    })
}
