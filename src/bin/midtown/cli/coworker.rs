use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderArg {
    Claude,
    Codex,
    Zai,
}

impl From<ProviderArg> for midtown::auth::AuthProvider {
    fn from(value: ProviderArg) -> Self {
        match value {
            ProviderArg::Claude => midtown::auth::AuthProvider::Claude,
            ProviderArg::Codex => midtown::auth::AuthProvider::Codex,
            ProviderArg::Zai => midtown::auth::AuthProvider::Zai,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum CoworkerCommand {
    /// Call in a new coworker
    #[command(alias = "spawn")]
    CallIn {
        /// Resume the previous Claude session (passes --continue to claude)
        #[arg(long)]
        resume: bool,
        /// Initial prompt to send after calling in (avoids separate nudge step)
        #[arg(long, short)]
        prompt: Option<String>,
        /// Execution provider for this coworker
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
    },
    /// Send a coworker on a break
    Break {
        /// Name of the coworker to send on a break
        name: String,
    },
    /// List all coworkers
    List,
    /// View a coworker's current terminal output
    View {
        /// Name of the coworker to view
        name: String,
    },
    /// Nudge a coworker to check in
    Nudge {
        /// Name of the coworker to nudge
        name: String,
        /// Custom message (optional)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Take a screenshot of a URL using Playwright
    Screenshot {
        /// URL to screenshot
        url: String,
        /// Output filename (default: screenshot.png)
        #[arg(long, short)]
        output: Option<String>,
        /// Label as a "before" screenshot (auto-names file)
        #[arg(long, conflicts_with = "after")]
        before: bool,
        /// Label as an "after" screenshot (auto-names file)
        #[arg(long, conflicts_with = "before")]
        after: bool,
    },
}

pub fn handle(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        CoworkerCommand::CallIn {
            resume,
            prompt,
            provider,
        } => {
            let resolved_provider = provider.map(Into::into).unwrap_or_else(|| {
                let project_name = midtown::paths::detect_repo_name().unwrap_or_default();
                midtown::config::get_execution_provider_for_role(
                    &project_name,
                    midtown::config::ExecutionRole::Coworker,
                )
            });
            client.coworker_spawn(*resume, prompt.as_deref(), resolved_provider)
        }
        CoworkerCommand::Break { name } => client.coworker_break(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::View { name } => handle_view(name, client),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
        CoworkerCommand::Screenshot { .. } => {
            // Handled before daemon connection in main.rs
            unreachable!("Screenshot is handled locally without daemon connection")
        }
    }
}

fn handle_view(name: &str, client: &DaemonClient) -> Result<Response, String> {
    // Get the rich-text output from the daemon, then render it to ANSI for the
    // terminal so users see formatted output instead of raw markdown syntax.
    let response = client.coworker_view(name)?;
    let raw = match response {
        Response::Message { message } => message,
        other => return Ok(other),
    };
    let rendered = super::session_render::render_ansi(&raw);
    Ok(Response::message(rendered.trim_end().to_string()))
}

/// RAII guard that removes a temp file on drop, ensuring cleanup on all exit paths.
pub(crate) struct TempFileGuard {
    pub(crate) path: std::path::PathBuf,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Take a screenshot of a URL using Playwright, upload it, and return
/// the `[Attached: /path]` markdown ready for channel posts or PR bodies.
///
/// Does not require a daemon connection — runs Playwright locally and
/// uploads via HTTP to the webhook port.
pub fn handle_screenshot(
    url: &str,
    output: Option<&str>,
    before: bool,
    after: bool,
) -> Result<Response, String> {
    // Determine output filename
    let filename = if let Some(name) = output {
        name.to_string()
    } else if before {
        "before.png".to_string()
    } else if after {
        "after.png".to_string()
    } else {
        "screenshot.png".to_string()
    };

    // Write to a unique temp file (PID-scoped to avoid clobbering from concurrent coworkers)
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join(format!(
        "midtown-screenshot-{}-{}",
        std::process::id(),
        filename
    ));

    // Run Playwright to capture the screenshot
    eprintln!("Capturing screenshot of {}...", url);
    let playwright_output = std::process::Command::new("npx")
        .args(["playwright@latest", "screenshot", "--browser", "chromium"])
        .arg(url)
        .arg(&tmp_path)
        .output()
        .map_err(|e| format!("Failed to run npx playwright: {}. Is Node.js installed?", e))?;

    if !playwright_output.status.success() {
        let stderr = String::from_utf8_lossy(&playwright_output.stderr);
        let stdout = String::from_utf8_lossy(&playwright_output.stdout);
        return Err(format!(
            "Playwright screenshot failed:\n{}\n{}",
            stderr.trim(),
            stdout.trim()
        ));
    }

    // Verify the file was created
    if !tmp_path.exists() {
        return Err("Playwright did not produce a screenshot file".to_string());
    }

    upload_and_cleanup(&tmp_path, &filename)
}

/// Upload a screenshot file and clean up the temp file afterward.
///
/// Creates a `TempFileGuard` over `tmp_path` so the file is removed on all exit
/// paths (success, early error return, or panic). Resolves the webhook port from
/// env/config, uploads via multipart POST, and returns `[Attached: /path]` markdown.
pub(crate) fn upload_and_cleanup(
    tmp_path: &std::path::Path,
    filename: &str,
) -> Result<Response, String> {
    // Guard ensures temp file is cleaned up on all exit paths (including early returns)
    let _guard = TempFileGuard {
        path: tmp_path.to_path_buf(),
    };

    // Resolve the daemon's webhook port:
    //   1. MIDTOWN_WEBHOOK_PORT env var (set by daemon for coworker sessions)
    //   2. Project config daemon.webhook_port (persisted by assign_webhook_port)
    //   3. Default (47023)
    let webhook_port = std::env::var("MIDTOWN_WEBHOOK_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or_else(|| {
            midtown::paths::detect_repo_name()
                .map(|repo| midtown::config::get_project_daemon_config(&repo))
                .and_then(|cfg| cfg.webhook_port)
                .unwrap_or(midtown::daemon::DEFAULT_WEBHOOK_PORT)
        });
    let upload_url = format!("http://127.0.0.1:{}/api/upload", webhook_port);

    let file_bytes =
        std::fs::read(tmp_path).map_err(|e| format!("Failed to read screenshot file: {}", e))?;

    let form = reqwest::blocking::multipart::Form::new().part(
        "file",
        reqwest::blocking::multipart::Part::bytes(file_bytes)
            .file_name(filename.to_string())
            .mime_str("image/png")
            .map_err(|e| format!("Failed to set MIME type: {}", e))?,
    );

    let client = reqwest::blocking::Client::new();
    let response = client
        .post(&upload_url)
        .multipart(form)
        .send()
        .map_err(|e| format!("Failed to upload screenshot: {}. Is the daemon running?", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(format!("Upload failed (HTTP {}): {}", status, body));
    }

    let upload_result: serde_json::Value = response
        .json()
        .map_err(|e| format!("Failed to parse upload response: {}", e))?;

    let path = upload_result
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Upload response missing 'path' field")?;

    // Return the [Attached: /path] markdown
    // (temp file cleanup happens automatically via TempFileGuard drop)
    let attached = format!("[Attached: {}]", path);
    eprintln!("Screenshot uploaded: {}", path);
    Ok(Response::message(attached))
}

/// Boot a headed (interactive terminal) coworker session for a task.
///
/// When `task_id` is provided, boots directly for that task. When `None`,
/// shows an interactive TUI picker for unresolved tasks. Creates/reuses a
/// task worktree, then `exec()`s into the `claude` CLI.
pub fn handle_coworker_boot(task_id: Option<&str>) -> Result<(), String> {
    use std::os::unix::process::CommandExt;

    let repo_name = midtown::paths::detect_repo_name()
        .ok_or("Not in a git repository. Run from a git repo.")?;

    // Task selection: explicit ID or interactive picker
    let task = if let Some(id) = task_id {
        midtown::tasks::read_task(id).ok_or_else(|| format!("Task !{} not found", id))?
    } else {
        select_task()?
    };

    eprintln!("Booting coworker for task !{}: {}", task.id, task.subject);

    // Create/reuse the task worktree
    let worktree_id = midtown::worktree_registry::branch_slug_for_task(&task.id, &task.subject);
    let wt_manager = midtown::worktree::WorktreeManager::from_current_dir()
        .map_err(|e| format!("Failed to initialize worktree manager: {}", e))?;
    let worktree_path = wt_manager
        .create_task_worktree(&worktree_id)
        .map_err(|e| format!("Failed to create worktree: {}", e))?;

    // Build launch config — use worktree_id as the coworker name for session identity
    let mut config = midtown::launch::LaunchConfig::coworker(
        &worktree_id,
        &repo_name,
        midtown::launch::SessionMode::Resume,
        None,
    );
    config.working_dir = Some(worktree_path.clone());

    // Resolve auth profile
    let profile_dir = midtown::auth::active_profile_dir_for_project_with_provider(
        &repo_name,
        config.auth_provider,
    );
    config.auth_profile_dir = Some(profile_dir);

    // Generate system prompt and write to temp file
    let system_prompt = midtown::agents::coworker_system_prompt(&worktree_id, &repo_name);
    let prompt_file =
        std::env::temp_dir().join(format!("midtown-coworker-prompt-{}.md", std::process::id()));
    std::fs::write(&prompt_file, &system_prompt)
        .map_err(|e| format!("Failed to write system prompt: {}", e))?;

    let settings_file = midtown::settings::write_coworker_settings_file()
        .map_err(|e| format!("Failed to write settings: {}", e))?;

    // Write task-specific initial prompt to temp file
    let initial_prompt = midtown::agents::coworker_task_prompt(&task.id, &task.subject, "");
    let initial_prompt_file = std::env::temp_dir().join(format!(
        "midtown-coworker-initial-{}.md",
        std::process::id()
    ));
    std::fs::write(&initial_prompt_file, &initial_prompt)
        .map_err(|e| format!("Failed to write initial prompt: {}", e))?;

    // Ensure plugins/skills are installed before launching
    if let Err(e) = midtown::platform_launch::run_platform_prelaunch_hook(config.auth_provider) {
        eprintln!(
            "Warning: Platform pre-launch hook failed (continuing): {}",
            e
        );
    }

    // Build the full shell command (env vars + sandbox + CLI args)
    let launch = config.to_shell_command(
        &settings_file,
        &prompt_file,
        Some(&initial_prompt_file),
        &worktree_path,
        &repo_name,
    );

    // exec() replaces this process — runs claude in the task worktree
    let err = std::process::Command::new("sh")
        .arg("-lc")
        .arg(&launch.shell_command)
        .current_dir(&worktree_path)
        .exec();
    Err(format!("Failed to exec: {}", err))
}

/// Interactive task picker for bare `midtown coworker` (no --task flag).
///
/// Reads all tasks, filters to unresolved (pending/in_progress), and presents
/// a numbered list on stderr. Same UX pattern as `choose_attach_candidate()`
/// in session.rs.
fn select_task() -> Result<midtown::tasks::Task, String> {
    let mut tasks: Vec<midtown::tasks::Task> = midtown::tasks::read_tasks()
        .into_iter()
        .filter(|t| t.status != midtown::tasks::TaskStatus::Completed)
        .collect();

    if tasks.is_empty() {
        return Err("No unresolved tasks. Create tasks first, then run `midtown coworker`.".into());
    }

    // Sort by ID (numeric) for stable ordering
    tasks.sort_by(|a, b| {
        a.id.parse::<u64>()
            .unwrap_or(0)
            .cmp(&b.id.parse::<u64>().unwrap_or(0))
    });

    if tasks.len() == 1 {
        eprintln!("Auto-selecting task !{}: {}", tasks[0].id, tasks[0].subject);
        return Ok(tasks[0].clone());
    }

    eprintln!("Select a task:");
    for (idx, task) in tasks.iter().enumerate() {
        let status = match task.status {
            midtown::tasks::TaskStatus::Pending => "pending",
            midtown::tasks::TaskStatus::InProgress => "in_progress",
            midtown::tasks::TaskStatus::Completed => "completed",
        };
        let owner_suffix = task
            .owner
            .as_deref()
            .filter(|o| !o.is_empty())
            .map(|o| format!(" ({})", o))
            .unwrap_or_default();
        eprintln!(
            "  {}. !{} [{}] {}{}",
            idx + 1,
            task.id,
            status,
            task.subject,
            owner_suffix
        );
    }

    loop {
        use std::io::Write;
        eprint!("Choice [1-{}]: ", tasks.len());
        std::io::stderr()
            .flush()
            .map_err(|e| format!("Failed to flush: {}", e))?;

        let mut input = String::new();
        let bytes_read = std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| format!("Failed to read input: {}", e))?;

        // EOF (Ctrl+D, piped input ended) — exit rather than spinning forever
        if bytes_read == 0 {
            return Err("No input (EOF). Run with --task <id> to specify a task.".into());
        }

        let trimmed = input.trim();
        if let Ok(index) = trimmed.parse::<usize>()
            && (1..=tasks.len()).contains(&index)
        {
            return Ok(tasks[index - 1].clone());
        }
        eprintln!("Enter a number between 1 and {}.", tasks.len());
    }
}

#[path = "coworker_tests.rs"]
#[cfg(test)]
mod tests;
