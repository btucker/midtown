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
        /// API key for non-interactive login (z.ai only)
        #[arg(long)]
        key: Option<String>,
    },
    /// List all profiles and interactively switch
    List,
    /// Switch to a different profile
    Switch {
        /// Profile name to switch to
        profile: String,

        /// Switch only the current project (global is the default)
        #[arg(long, default_value_t = false, conflicts_with = "all")]
        project: bool,

        /// Deprecated alias for global scope (global is already the default)
        #[arg(long, hide = true, default_value_t = false, conflicts_with = "project")]
        all: bool,
    },
    /// Remove a profile
    Remove {
        /// Profile name to remove
        profile: String,
    },
}

pub fn handle(
    cmd: &AuthCommand,
    provider: midtown::auth::AuthProvider,
) -> Result<Response, String> {
    match cmd {
        AuthCommand::Login { email, key } => handle_login(email, key.as_deref(), provider),
        AuthCommand::List => handle_list(provider),
        AuthCommand::Switch {
            profile,
            project,
            all,
        } => handle_switch(profile, use_global_scope(*project, *all), provider),
        AuthCommand::Remove { profile } => handle_remove(profile, provider),
    }
}

fn use_global_scope(project: bool, all: bool) -> bool {
    !project || all
}

fn should_apply_global_already_on_fallback(global: bool, response: &Response) -> bool {
    global
        && matches!(
            response,
            Response::Message { message } if message.starts_with("Already on ")
        )
}

pub fn handle_list_all_providers() -> Result<Response, String> {
    let mut sections = Vec::new();

    for provider in midtown::auth::AuthProvider::all() {
        let (rows, context) = fetch_sorted_profiles(*provider)?;
        let note = context.header_line(*provider);
        if rows.is_empty() {
            sections.push(format!(
                "{}\n  No profiles found. Create one with: midtown auth --provider {} login <email>",
                provider, provider
            ));
            continue;
        }
        let table = format_table(&rows);
        let body = match note {
            Some(line) => format!("{}\n{}", line, table),
            None => table,
        };
        sections.push(format!("{}\n{}", provider, body));
    }

    Ok(Response::Message {
        message: sections.join("\n\n"),
    })
}

fn handle_login(
    email: &str,
    api_key: Option<&str>,
    provider: midtown::auth::AuthProvider,
) -> Result<Response, String> {
    // Validate email format (must contain @)
    if !email.contains('@') {
        return Err(format!(
            "Invalid email '{}'. Use an email address (e.g., user@example.com).",
            email
        ));
    }

    let profile_dir = midtown::auth::ensure_profile_dir_for(provider, email)
        .map_err(|e| format!("Failed to create profile directory: {}", e))?;

    // Handle z.ai non-interactive login with API key
    if provider == midtown::auth::AuthProvider::Zai {
        if let Some(key) = api_key {
            // Non-interactive: write API key to file
            let key_file = profile_dir.join("api_key.txt");
            std::fs::write(&key_file, format!("{}\n", key))
                .map_err(|e| format!("Failed to write API key: {}", e))?;

            // Set file permissions to 600 (owner read/write only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("Failed to set key file permissions: {}", e))?;
            }

            // If this is the first profile, set it as current
            let profiles = midtown::auth::list_profiles_for(provider).unwrap_or_default();
            if profiles.len() == 1
                && let Err(e) = midtown::auth::set_current_profile_for(provider, email)
            {
                eprintln!(
                    "Warning: Could not set '{}' as current profile: {}",
                    email, e
                );
            }

            return Ok(Response::Message {
                message: format!("Profile '{}' configured for z.ai.", email),
            });
        } else {
            // Interactive: prompt for API key
            println!("z.ai authentication setup for profile '{}'", email);
            println!("Config dir: {}", profile_dir.display());
            println!();
            eprint!("Enter API key: ");
            std::io::Write::flush(&mut std::io::stderr())
                .map_err(|e| format!("Failed to flush stderr: {}", e))?;

            let mut key_input = String::new();
            std::io::stdin()
                .read_line(&mut key_input)
                .map_err(|e| format!("Failed to read API key: {}", e))?;
            let key = key_input.trim();

            if key.is_empty() {
                return Err("API key cannot be empty".to_string());
            }

            // Write API key to file
            let key_file = profile_dir.join("api_key.txt");
            std::fs::write(&key_file, format!("{}\n", key))
                .map_err(|e| format!("Failed to write API key: {}", e))?;

            // Set file permissions to 600 (owner read/write only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("Failed to set key file permissions: {}", e))?;
            }

            // Optional: prompt for base URL
            println!();
            eprint!("Enter base URL (press Enter for default https://api.z.ai/api/anthropic): ");
            std::io::Write::flush(&mut std::io::stderr())
                .map_err(|e| format!("Failed to flush stderr: {}", e))?;

            let mut base_url_input = String::new();
            std::io::stdin()
                .read_line(&mut base_url_input)
                .map_err(|e| format!("Failed to read base URL: {}", e))?;
            let base_url = base_url_input.trim();

            if !base_url.is_empty() {
                let base_url_file = profile_dir.join("base_url.txt");
                std::fs::write(&base_url_file, format!("{}\n", base_url))
                    .map_err(|e| format!("Failed to write base URL: {}", e))?;
            }

            // If this is the first profile, set it as current
            let profiles = midtown::auth::list_profiles_for(provider).unwrap_or_default();
            if profiles.len() == 1
                && let Err(e) = midtown::auth::set_current_profile_for(provider, email)
            {
                eprintln!(
                    "Warning: Could not set '{}' as current profile: {}",
                    email, e
                );
            }

            return Ok(Response::Message {
                message: format!("Profile '{}' configured for z.ai.", email),
            });
        }
    }

    // Original logic for Claude and Codex
    println!(
        "Launching {} with profile '{}'...",
        provider.cli_command(),
        email
    );
    println!("Config dir: {}", profile_dir.display());
    println!();
    println!(
        "Run login/auth commands inside the {} session to authenticate.",
        provider.cli_command()
    );
    println!("Once authenticated, exit the session. The tokens will be cached");
    println!("in {} for future use.", profile_dir.display());
    println!();

    let status = std::process::Command::new(provider.cli_command())
        .env(provider.env_var(), &profile_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| {
            format!(
                "Failed to launch {}: {}. Is {} installed?",
                provider.cli_command(),
                e,
                provider.cli_command()
            )
        })?;

    if !status.success() {
        return Err(format!(
            "{} exited with status: {}",
            provider.cli_command(),
            status
        ));
    }

    // If this is the first profile, set it as current
    let profiles = midtown::auth::list_profiles_for(provider).unwrap_or_default();
    if profiles.len() == 1
        && let Err(e) = midtown::auth::set_current_profile_for(provider, email)
    {
        eprintln!(
            "Warning: Could not set '{}' as current profile: {}",
            email, e
        );
    }

    Ok(Response::Message {
        message: format!(
            "Profile '{}' authenticated successfully for {}.",
            email, provider
        ),
    })
}

/// Data for a profile row in the interactive list.
struct ProfileRow {
    name: String,
    is_current: bool,
    is_global_current: bool,
    has_credentials: bool,
    usage: Option<midtown::UsageData>,
    /// Remaining capacity = min(100 - session_util, 100 - week_util).
    /// Higher is better. None for profiles without usage data.
    remaining_capacity: Option<f64>,
    /// Soonest bottleneck reset time (for tiebreaking at 0% remaining).
    bottleneck_reset: Option<chrono::DateTime<chrono::Utc>>,
}

struct ProfileListContext {
    project_name: Option<String>,
    active_profile: String,
    global_profile: String,
}

impl ProfileListContext {
    fn header_line(&self, provider: midtown::auth::AuthProvider) -> Option<String> {
        if let Some(project) = &self.project_name
            && self.active_profile != self.global_profile
        {
            return Some(format!(
                "Active {} profile for project '{}': {} (global default: {})",
                provider, project, self.active_profile, self.global_profile
            ));
        }
        None
    }
}

/// Fetch profiles with usage data, sorted by available capacity (best first).
fn fetch_sorted_profiles(
    provider: midtown::auth::AuthProvider,
) -> Result<(Vec<ProfileRow>, ProfileListContext), String> {
    let profiles = midtown::auth::list_profiles_for(provider)
        .map_err(|e| format!("Failed to list profiles for {}: {}", provider, e))?;

    let global_current = midtown::auth::current_profile_for(provider);
    let project_name = midtown::paths::detect_repo_name();
    let active_current = if let Some(project) = &project_name {
        midtown::auth::active_profile_for_project_with_provider(project, provider)
    } else {
        global_current.clone()
    };

    // Fetch usage data for all authenticated profiles in parallel
    let usage_results: Vec<(String, Option<midtown::UsageData>)> =
        if provider == midtown::auth::AuthProvider::Claude {
            std::thread::scope(|s| {
                let handles: Vec<_> = profiles
                    .iter()
                    .map(|name| {
                        let name = name.clone();
                        s.spawn(move || {
                            let usage = midtown::fetch_usage_for_profile(&name);
                            (name, usage)
                        })
                    })
                    .collect();
                handles.into_iter().filter_map(|h| h.join().ok()).collect()
            })
        } else {
            profiles
                .iter()
                .map(|name| (name.clone(), None))
                .collect::<Vec<_>>()
        };

    let mut rows: Vec<ProfileRow> = profiles
        .iter()
        .map(|name| {
            let status = midtown::auth::profile_status_for(provider, name);
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
                is_current: *name == active_current,
                is_global_current: *name == global_current,
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

    Ok((
        rows,
        ProfileListContext {
            project_name,
            active_profile: active_current,
            global_profile: global_current,
        },
    ))
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

fn handle_list(provider: midtown::auth::AuthProvider) -> Result<Response, String> {
    let (rows, context) = fetch_sorted_profiles(provider)?;
    let note = context.header_line(provider);

    // Non-TTY: static table only
    if !std::io::stdout().is_terminal() {
        if rows.is_empty() {
            return Ok(Response::Message {
                message: format!(
                    "No {} profiles found. Create one with: midtown auth --provider {} login <email>",
                    provider, provider
                ),
            });
        }
        let table = format_table(&rows);
        let message = match note {
            Some(line) => format!("{}\n{}", line, table),
            None => table,
        };
        return Ok(Response::Message { message });
    }

    // Print the table first so usage details are visible above the selector
    if rows.is_empty() {
        println!("No profiles found.\n");
    } else {
        if let Some(line) = note {
            println!("{}", line);
        }
        println!("{}", format_table(&rows));
        println!();
    }

    // Interactive selector below the table
    let action = run_interactive_selector(&rows)?;

    match action {
        SelectorAction::Switch(profile) => {
            let scope = run_scope_selector()?;
            match scope {
                Some(scope_global) => handle_switch(&profile, scope_global, provider),
                None => Ok(Response::Message {
                    message: String::new(),
                }), // cancelled
            }
        }
        SelectorAction::AddAccount => prompt_add_account(provider),
        SelectorAction::Remove(profile) => confirm_and_remove(&profile, provider),
        SelectorAction::Cancel => Ok(Response::Message {
            message: String::new(),
        }),
    }
}

/// Confirm and remove a profile.
fn confirm_and_remove(
    profile: &str,
    provider: midtown::auth::AuthProvider,
) -> Result<Response, String> {
    eprint!("Remove profile '{}'? [y/N] ", profile);
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read input: {}", e))?;
    if input.trim().eq_ignore_ascii_case("y") {
        handle_remove(profile, provider)
    } else {
        Ok(Response::Message {
            message: "Cancelled.".to_string(),
        })
    }
}

/// Prompt the user for an email and run the login flow.
fn prompt_add_account(provider: midtown::auth::AuthProvider) -> Result<Response, String> {
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
    handle_login(email, None, provider)
}

/// Format profiles as a static table string.
fn format_table(rows: &[ProfileRow]) -> String {
    let mut lines = Vec::new();
    let has_distinct_global_marker = rows.iter().any(|r| r.is_global_current && !r.is_current);

    // Build display rows: (profile, session_usage, session_resets, week_usage, week_resets)
    let display_rows: Vec<(String, String, String, String, String)> = rows
        .iter()
        .map(|row| {
            let marker = if row.is_current {
                " *"
            } else if row.is_global_current {
                " ^"
            } else {
                ""
            };
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

    if has_distinct_global_marker {
        lines.push(String::new());
        lines.push("* active for this context, ^ global default".to_string());
    }

    lines.join("\n")
}

/// RAII guard that ensures raw mode is disabled even on panic.
struct RawModeGuard;

impl RawModeGuard {
    fn new() -> Result<Self, String> {
        enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {}", e))?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Run the interactive TUI selector inline below the cursor.
fn run_interactive_selector(rows: &[ProfileRow]) -> Result<SelectorAction, String> {
    // Find current profile's index for pre-selection (or 0 for the "+ Add account" row)
    let current_idx = rows.iter().position(|r| r.is_current).unwrap_or(0);
    let mut state = ListState::default();
    state.select(Some(current_idx));

    // +3 = 1 for "Add account" row + 2 for borders
    let viewport_height = rows.len() as u16 + 3;

    let _guard = RawModeGuard::new()?;
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

    // _guard dropped here restores terminal via disable_raw_mode()
    result
}

/// Inline selector for switch scope: all projects (default) or this project only.
/// Returns Some(true) for global, Some(false) for project scope, None for cancel.
fn run_scope_selector() -> Result<Option<bool>, String> {
    let options = ["All projects (default)", "This project only"];
    let mut state = ListState::default();
    state.select(Some(0));

    // +2 for borders
    let viewport_height = options.len() as u16 + 2;

    let _guard = RawModeGuard::new()?;
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
                    let global = state.selected() == Some(0);
                    break Ok(Some(global));
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

    // _guard dropped here restores terminal via disable_raw_mode()
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
                        if i < rows.len() {
                            let row = &rows[i];
                            if row.is_current {
                                return Ok(SelectorAction::Cancel);
                            }
                            return Ok(SelectorAction::Switch(row.name.clone()));
                        }
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

fn handle_switch(
    profile: &str,
    global: bool,
    provider: midtown::auth::AuthProvider,
) -> Result<Response, String> {
    // If the daemon is running, use RPC to switch profile and re-launch all claudes.
    // This ensures running coworkers and the lead pick up the new credentials.
    if let Ok(client) = crate::client::DaemonClient::connect() {
        let response = client.auth_switch(profile, global, provider)?;
        if should_apply_global_already_on_fallback(global, &response) {
            // Compatibility fallback for older daemons that short-circuit before
            // clearing project overrides on global switches.
            let cleared = midtown::config::clear_all_project_auth_profiles_for(provider);
            if cleared > 0 {
                let msg = match &response {
                    Response::Message { message } => message.as_str(),
                    _ => "Already on selected profile.",
                };
                return Ok(Response::Message {
                    message: format!(
                        "{} Cleared {} project override(s) for {} so global profile '{}' now applies.",
                        msg, cleared, provider, profile
                    ),
                });
            }
        }
        return Ok(response);
    }

    if global {
        // Global switch: update the global current profile and clear per-project overrides
        midtown::auth::set_current_profile_for(provider, profile).map_err(|e| e.to_string())?;
        midtown::config::clear_all_project_auth_profiles_for(provider);

        Ok(Response::Message {
            message: format!(
                "Switched all projects to {} profile '{}'. No daemon running, new sessions will use this profile.",
                provider, profile
            ),
        })
    } else {
        // Per-project switch: update current project's config
        let project_name = midtown::paths::detect_repo_name().ok_or_else(|| {
            "Not in a git repository. Omit --project to switch globally.".to_string()
        })?;

        set_project_auth_profile(&project_name, profile, provider)?;

        Ok(Response::Message {
            message: format!(
                "Switched project '{}' to {} profile '{}'. No daemon running, new sessions will use this profile.",
                project_name, provider, profile
            ),
        })
    }
}

/// Set the auth_profile in a project's config.toml.
fn set_project_auth_profile(
    project_name: &str,
    profile: &str,
    provider: midtown::auth::AuthProvider,
) -> Result<(), String> {
    let path = midtown::config::project_config_path(project_name);
    let mut config = midtown::config::FullProjectConfig::load_from(&path).unwrap_or_default();
    midtown::auth::set_project_profile_override(&mut config.project, provider, profile.to_string());
    config
        .save_to(&path)
        .map_err(|e| format!("Failed to save project config: {}", e))
}

fn handle_remove(profile: &str, provider: midtown::auth::AuthProvider) -> Result<Response, String> {
    midtown::auth::remove_profile_for(provider, profile).map_err(|e| e.to_string())?;

    Ok(Response::Message {
        message: format!("Removed {} profile '{}'.", provider, profile),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_table_marks_active_and_global_profiles() {
        let rows = vec![
            ProfileRow {
                name: "active@example.com".to_string(),
                is_current: true,
                is_global_current: false,
                has_credentials: true,
                usage: None,
                remaining_capacity: None,
                bottleneck_reset: None,
            },
            ProfileRow {
                name: "global@example.com".to_string(),
                is_current: false,
                is_global_current: true,
                has_credentials: true,
                usage: None,
                remaining_capacity: None,
                bottleneck_reset: None,
            },
        ];

        let table = format_table(&rows);
        assert!(table.contains("active@example.com *"));
        assert!(table.contains("global@example.com ^"));
        assert!(table.contains("* active for this context, ^ global default"));
    }

    #[test]
    fn context_header_only_when_project_override_differs() {
        let provider = midtown::auth::AuthProvider::Claude;
        let ctx = ProfileListContext {
            project_name: Some("midtown".to_string()),
            active_profile: "claude@quotably.com".to_string(),
            global_profile: "ben@quotably.com".to_string(),
        };
        assert!(
            ctx.header_line(provider)
                .unwrap()
                .contains("Active claude profile for project 'midtown'")
        );

        let same_ctx = ProfileListContext {
            project_name: Some("midtown".to_string()),
            active_profile: "ben@quotably.com".to_string(),
            global_profile: "ben@quotably.com".to_string(),
        };
        assert!(same_ctx.header_line(provider).is_none());
    }

    #[test]
    fn use_global_scope_defaults_to_global() {
        assert!(use_global_scope(false, false));
        assert!(use_global_scope(false, true));
        assert!(!use_global_scope(true, false));
    }

    #[test]
    fn fallback_applies_only_for_global_already_on_message() {
        let already_on = Response::Message {
            message: "Already on claude profile 'ben@quotably.com'".to_string(),
        };
        assert!(should_apply_global_already_on_fallback(true, &already_on));
        assert!(!should_apply_global_already_on_fallback(false, &already_on));

        let other = Response::Message {
            message: "Switched profile".to_string(),
        };
        assert!(!should_apply_global_already_on_fallback(true, &other));
    }
}
