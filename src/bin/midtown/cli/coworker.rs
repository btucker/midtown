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
