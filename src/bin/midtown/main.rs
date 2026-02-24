//! Midtown CLI - command-line interface for the midtown daemon.
//!
//! This binary provides user-facing commands for interacting with the daemon,
//! managing channels, coworkers, tasks, and pull requests.

use clap::{Parser, Subcommand};

mod cli;
mod client;

use cli::{
    AuthCommand, ChannelCommand, ConfigCommand, CoworkerCommand, DiagramCommand, E2eCommand,
    HeadedWrapperCommand, HookCommand, PrCommand, SessionCommand, TaskCommand,
};
use client::DaemonClient;

#[derive(Parser)]
#[command(name = "midtown")]
#[command(about = "Midtown - AI coworker orchestration", long_about = None)]
struct Cli {
    /// Output format (json or pretty)
    #[arg(long, default_value = "pretty")]
    format: OutputFormat,

    /// Path to git repository (defaults to current directory)
    #[arg(long, short = 'C', global = true)]
    repo: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum OutputFormat {
    Json,
    Pretty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum AuthProviderArg {
    Claude,
    Codex,
    Zai,
}

impl From<AuthProviderArg> for midtown::auth::AuthProvider {
    fn from(value: AuthProviderArg) -> Self {
        match value {
            AuthProviderArg::Claude => midtown::auth::AuthProvider::Claude,
            AuthProviderArg::Codex => midtown::auth::AuthProvider::Codex,
            AuthProviderArg::Zai => midtown::auth::AuthProvider::Zai,
        }
    }
}

#[derive(Subcommand, Clone)]
enum Commands {
    /// Run the daemon server (internal - use 'start' instead)
    #[command(hide = true)]
    Daemon {
        /// Path to the Unix socket
        #[arg(short, long)]
        socket: Option<std::path::PathBuf>,

        /// Working directory for coworkers
        #[arg(short, long)]
        workdir: Option<std::path::PathBuf>,

        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,

        /// Port for GitHub webhook server (disabled if not set)
        #[arg(long)]
        webhook_port: Option<u16>,

        /// Run in foreground (don't daemonize) - for debugging
        #[arg(long)]
        foreground: bool,

        /// Project name (overrides auto-detection)
        #[arg(long)]
        project: Option<String>,
    },
    /// Start midtown services (daemon + shared webserver)
    Start {
        /// Project name (overrides auto-detection)
        #[arg(long)]
        project: Option<String>,

        /// Additional repository paths to include
        #[arg(long = "add-repo")]
        repos: Vec<std::path::PathBuf>,
    },
    /// Stop midtown services
    Stop {
        /// Keep legacy midtown-* multiplexer sessions running if present
        #[arg(long)]
        keep_session: bool,
    },
    /// Restart midtown (stop + start)
    Restart {
        /// Skip waiting for active review coworkers to go on break before restart
        #[arg(long)]
        force: bool,
    },
    /// Launch chat in this terminal (chat-only by default; use --attach to open Lead in a split)
    #[command(alias = "attach")]
    View {
        /// Project name to view (default: inferred from cwd)
        project: Option<String>,

        /// Attach to the Lead session and open it in a split pane
        #[arg(long)]
        attach: bool,
    },
    /// Config management commands (get/set/list)
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Project management commands
    #[command(hide = true)]
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Channel messaging commands
    Channel {
        #[command(subcommand)]
        command: ChannelCommand,
    },
    /// Coworker management commands
    Coworker {
        #[command(subcommand)]
        command: CoworkerCommand,
    },
    /// Attach/detach headless coworker sessions
    #[command(alias = "sessions")]
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Task management commands
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Show system status
    Status,
    /// Headed wrapper intercom (register/poll/ack + PTY-backed run-agent)
    #[command(hide = true)]
    HeadedWrapper {
        #[command(subcommand)]
        command: HeadedWrapperCommand,
    },
    /// Pull request commands
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Lead-specific commands
    #[command(hide = true)]
    Lead {
        /// Channel to lead (defaults to main project channel)
        #[arg(long)]
        channel: Option<String>,
        #[command(subcommand)]
        command: Option<LeadCommand>,
    },
    /// Open IRC-style chat TUI
    #[command(hide = true)]
    Chat,
    /// E2E testing commands (auth setup, run containerized tests)
    #[command(hide = true)]
    E2e {
        #[command(subcommand)]
        command: E2eCommand,
    },
    /// Manage authentication profiles for multi-account support
    Auth {
        /// Auth provider to manage (prompts if not specified for switch/remove)
        #[arg(long, value_enum)]
        provider: Option<AuthProviderArg>,

        #[command(subcommand)]
        command: Option<AuthCommand>,
    },
    /// Report coworker workflow state (called by coworkers to update status)
    #[command(hide = true)]
    State {
        /// Workflow phase
        #[arg(value_enum)]
        phase: midtown::coworker_state::WorkflowPhase,

        /// Task number being worked on
        #[arg(long)]
        task: Option<u32>,

        /// Progress percentage (0-100)
        #[arg(long)]
        progress: Option<u8>,

        /// PR number associated with this task (links task.pr for auto-completion on merge)
        #[arg(long)]
        pr: Option<u64>,
    },
    /// Hook handlers (insight, idle, task, ask) - called by Claude Code hooks
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        command: HookCommand,
    },
    /// Diagram utilities (validation, rendering)
    #[command(hide = true)]
    Diagram {
        #[command(subcommand)]
        command: DiagramCommand,
    },
    /// View daemon or hook logs
    #[command(hide = true)]
    Log {
        /// Show hooks log instead of daemon log
        #[arg(long)]
        hooks: bool,

        /// Print the log file path instead of tailing
        #[arg(long)]
        path: bool,

        /// Follow log output (default: true)
        #[arg(short, long, default_value = "true")]
        follow: bool,

        /// Number of lines to show initially
        #[arg(short = 'n', long, default_value = "50")]
        lines: u32,
    },
    /// Standalone multi-project webserver
    #[command(hide = true)]
    Webserver {
        #[command(subcommand)]
        command: WebserverCommand,
    },
    /// Run Claude Code using the current midtown auth profile
    #[command(hide = true)]
    Claude {
        /// Additional arguments to pass to the claude CLI
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run a one-shot Claude Code session via the daemon (JSON streaming)
    #[command(hide = true)]
    Oneshot {
        /// Prompt to send to Claude
        prompt: String,

        /// Model to use (e.g., sonnet, opus, haiku)
        #[arg(long, default_value = "sonnet")]
        model: String,

        /// System prompt for the session
        #[arg(long, default_value = "")]
        system_prompt: String,

        /// JSON schema for structured output validation
        #[arg(long)]
        json_schema: Option<String>,

        /// Maximum budget in USD
        #[arg(long)]
        max_budget_usd: Option<f64>,

        /// Allow tool use (default: no tools)
        #[arg(long)]
        allow_tools: bool,
    },
}

#[derive(Subcommand, Clone)]
enum WebserverCommand {
    /// Run the webserver (default)
    Run {
        /// Port to listen on (default: 47022)
        #[arg(long)]
        port: Option<u16>,

        /// Path to static web assets directory
        #[arg(long)]
        static_dir: Option<std::path::PathBuf>,

        /// Run in foreground (don't daemonize) - for debugging
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the webserver
    Stop,
    /// Restart the webserver
    Restart,
}

#[derive(Subcommand, Clone)]
enum LeadCommand {
    /// Register this session for task sharing with coworkers
    #[command(hide = true)]
    RegisterSession,
    /// Manage reminders (condition-based notifications)
    Remind {
        #[command(subcommand)]
        command: RemindCommand,
    },
}

#[derive(Subcommand, Clone)]
enum RemindCommand {
    /// Set a reminder for when all tasks are done and all PRs are merged
    AllWorkMerged {
        /// Message to display when the reminder fires
        message: String,
    },
    /// List active reminders
    List,
    /// Cancel a reminder by ID
    Cancel {
        /// Reminder ID to cancel
        id: String,
    },
}

#[derive(Subcommand, Clone)]
enum ProjectCommand {
    /// List all known projects and their status
    List,
}

fn main() {
    let cli = Cli::parse();
    let format = cli.format;

    // Handle --repo option: change to specified directory
    if let Some(repo_path) = &cli.repo
        && let Err(e) = std::env::set_current_dir(repo_path)
    {
        eprintln!(
            "Error: Failed to change to repo directory '{}': {}",
            repo_path.display(),
            e
        );
        std::process::exit(1);
    }

    // Default to Start if no command provided
    let command = cli.command.unwrap_or(Commands::Start {
        project: None,
        repos: vec![],
    });

    // Daemon command (runs the daemon server - internal use)
    if let Commands::Daemon {
        socket,
        workdir,
        verbose,
        webhook_port,
        foreground,
        project,
    } = &command
    {
        let mut config = midtown::daemon::DaemonConfig::default();
        if let Some(s) = socket {
            config.socket_path = s.clone();
        }
        if let Some(w) = workdir {
            config.workdir = w.clone();
        }
        config.verbose = *verbose;
        // CLI flag overrides env var
        if webhook_port.is_some() {
            config.webhook_port = *webhook_port;
        }
        if let Some(p) = project {
            config.project_name = Some(p.clone());
        }

        // Daemonize unless --foreground is set
        if !foreground {
            use daemonize::Daemonize;
            use std::os::unix::net::UnixStream;

            // Check if daemon is already running BEFORE forking
            // This allows us to print the error to the terminal
            if config.socket_path.exists() && UnixStream::connect(&config.socket_path).is_ok() {
                // Try to get the PID for a helpful message
                let pid_msg = std::fs::read_to_string(&config.pid_file_path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
                    .map(|pid| format!(" (PID {})", pid))
                    .unwrap_or_default();
                eprintln!(
                    "Error: Daemon is already running{}. Stop it first with 'midtown stop'.",
                    pid_msg
                );
                std::process::exit(1);
            }

            // Create log directory for daemon output
            let log_dir = midtown::paths::daemon_log_dir();
            if let Err(e) = std::fs::create_dir_all(&log_dir) {
                eprintln!("Failed to create log directory: {}", e);
                std::process::exit(1);
            }

            // Open log files for stdout/stderr
            let stdout_path = log_dir.join("daemon.out");
            let stderr_path = log_dir.join("daemon.err");

            let stdout = match std::fs::File::options()
                .append(true)
                .create(true)
                .open(&stdout_path)
            {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Failed to open stdout log: {}", e);
                    std::process::exit(1);
                }
            };
            let stderr = match std::fs::File::options()
                .append(true)
                .create(true)
                .open(&stderr_path)
            {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Failed to open stderr log: {}", e);
                    std::process::exit(1);
                }
            };

            // Write startup separator to both logs so runs are distinguishable
            use std::io::Write;
            let separator = format!(
                "\n=== Daemon started at {} ===\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
            );
            // Best-effort writes; if these fail, the daemon will still start
            let _ = (&stdout).write_all(separator.as_bytes());
            let _ = (&stderr).write_all(separator.as_bytes());

            // Note: We don't use daemonize's pid_file feature because we have
            // our own PID file with flock-based locking for singleton enforcement.
            // The daemon::run() function handles the PID file.
            let daemonize = Daemonize::new()
                .working_directory(&config.workdir)
                .stdout(stdout)
                .stderr(stderr);

            match daemonize.start() {
                Ok(_) => {
                    // We are now in the daemon child process
                    // Continue to run the daemon server below
                }
                Err(e) => {
                    eprintln!("Failed to daemonize: {}", e);
                    std::process::exit(1);
                }
            }
        }

        // Run the daemon (this blocks until shutdown or exec-restart)
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        match rt.block_on(midtown::daemon::run(config)) {
            Ok(midtown::daemon::DaemonExitStatus::Shutdown) => {
                // Normal shutdown — exit cleanly
                return;
            }
            Ok(midtown::daemon::DaemonExitStatus::ExecRestart {
                workdir,
                project_name,
            }) => {
                // Drop the tokio runtime before exec to release resources
                drop(rt);

                // Re-exec the daemon binary with --foreground to avoid re-daemonizing.
                // This preserves the original process context (PID, sandbox state),
                // which is critical: if the original daemon was launched from an
                // unsandboxed context, the re-exec'd daemon stays unsandboxed and
                // can properly sandbox coworkers with sandbox-exec.
                let exe = std::env::current_exe().unwrap_or_else(|e| {
                    eprintln!("Failed to get current executable for exec-restart: {}", e);
                    std::process::exit(1);
                });

                use std::os::unix::process::CommandExt;
                let mut cmd = std::process::Command::new(&exe);
                cmd.arg("daemon");
                cmd.arg("--foreground");
                cmd.arg("--workdir").arg(&workdir);
                if let Some(ref project) = project_name {
                    cmd.arg("--project").arg(project);
                }

                eprintln!(
                    "\n=== Daemon exec-restart at {} ===",
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
                );

                // exec() replaces this process — this line never returns on success
                let err = cmd.exec();
                eprintln!("Failed to exec daemon: {}", err);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Daemon error: {}", e);
                std::process::exit(1);
            }
        }
    }

    // Start command (starts daemon, doesn't need existing connection)
    if let Commands::Start { project, repos } = &command {
        let result = cli::handle_start(project.clone(), repos.clone());
        handle_result(format, result);
        return;
    }

    // Stop command (stops daemon, doesn't need existing connection)
    if let Commands::Stop { keep_session } = &command {
        let result = cli::handle_stop(*keep_session);
        handle_result(format, result);
        return;
    }

    // Restart command (stop + start)
    if let Commands::Restart { force } = &command {
        let result = cli::handle_restart(*force);
        handle_result(format, result);
        return;
    }

    // View command (launches chat locally and best-effort split for Lead)
    if let Commands::View { project, attach } = &command {
        let result = cli::handle_view(project.as_deref(), *attach);
        handle_result(format, result);
        return;
    }

    // Project commands (no daemon required)
    if let Commands::Project { command: proj_cmd } = &command {
        let result = match proj_cmd {
            ProjectCommand::List => cli::handle_project_list(),
        };
        handle_result(format, result);
        return;
    }

    // Config commands (no daemon required — reads/writes config files directly)
    if let Commands::Config { command } = &command {
        let result = cli::handle_config(command);
        handle_result(format, result);
        return;
    }

    // Lead commands
    if let Commands::Lead { channel, command } = &command {
        if let Some(cmd) = command {
            let result = match cmd {
                LeadCommand::RegisterSession => cli::handle_register_session(),
                LeadCommand::Remind {
                    command: remind_cmd,
                } => match DaemonClient::connect() {
                    Ok(client) => cli::handle_remind(remind_cmd, &client),
                    Err(e) => Err(format!(
                        "Failed to connect to daemon: {}. Is it running?",
                        e
                    )),
                },
            };
            handle_result(format, result);
        } else {
            // No subcommand — print info about channel if specified
            if let Some(ch) = channel {
                println!("Channel lead mode: #{}", ch);
            }
            // midtown lead without subcommand is a no-op (sessions are managed by daemon)
        }
        return;
    }

    // Chat command (no daemon required - standalone TUI)
    if let Commands::Chat = &command {
        if let Err(e) = cli::handle_chat() {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // E2e command (no daemon required - auth setup or containerized tests)
    if let Commands::E2e { command } = &command {
        if let Err(e) = cli::handle_e2e(command) {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // Auth command (no daemon required - profile management)
    // Bare `midtown auth` defaults to `midtown auth list`
    if let Commands::Auth { provider, command } = &command {
        let cmd = command.clone().unwrap_or(AuthCommand::List);
        let result = if let Some(provider_arg) = provider {
            // Explicit provider specified: use single-provider handling
            cli::handle_auth(&cmd, (*provider_arg).into())
        } else {
            // No provider specified: behavior depends on command
            match cmd {
                // List shows all providers by default (no prompt needed)
                AuthCommand::List => cli::handle_auth_list_all_providers(),
                // Other commands prompt for provider selection
                _ => {
                    let provider = match cli::prompt_provider_selection_all() {
                        Ok(p) => p,
                        Err(e) => {
                            handle_result(format, Err(e));
                            return;
                        }
                    };
                    cli::handle_auth(&cmd, provider)
                }
            }
        };
        handle_result(format, result);
        return;
    }

    // State command (no daemon required - writes state file directly)
    if let Commands::State {
        phase,
        task,
        progress,
        pr,
    } = &command
    {
        let result = cli::handle_state(*phase, *task, *progress, *pr);
        handle_result(format, result);
        return;
    }

    // Hook commands (no daemon required - called by Claude Code hooks)
    if let Commands::Hook { command } = &command {
        let result = cli::handle_hook(command);
        handle_result(format, result);
        return;
    }

    // Diagram commands (no daemon required - uses selkie library directly)
    if let Commands::Diagram { command } = &command {
        let result = cli::handle_diagram(command);
        handle_result(format, result);
        return;
    }

    // Log command (no daemon required - just tails log files)
    if let Commands::Log {
        hooks,
        path,
        follow,
        lines,
    } = &command
    {
        let log_path = if *hooks {
            midtown::paths::hooks_log_file()
        } else {
            midtown::paths::daemon_log_file()
        };

        if *path {
            println!("{}", log_path.display());
            return;
        }

        if !log_path.exists() {
            eprintln!("Log file not found: {}", log_path.display());
            eprintln!("Is the daemon running? Try: midtown start");
            std::process::exit(1);
        }

        // exec into tail — replaces this process so the user gets proper signal handling
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new("tail");
        if *follow {
            cmd.arg("-f");
        }
        cmd.arg("-n").arg(lines.to_string()).arg(&log_path);
        let err = cmd.exec();
        eprintln!("Failed to exec tail: {}", err);
        std::process::exit(1);
    }

    // Webserver commands (standalone, no daemon required)
    if let Commands::Webserver { command: ws_cmd } = &command {
        match ws_cmd {
            WebserverCommand::Stop => {
                let result = cli::handle_webserver_stop();
                handle_result(format, result);
                return;
            }
            WebserverCommand::Restart => {
                let result = cli::handle_webserver_restart();
                handle_result(format, result);
                return;
            }
            WebserverCommand::Run {
                port,
                static_dir,
                foreground,
            } => {
                let mut config = midtown::webserver::WebserverConfig::default();
                if let Some(p) = port {
                    config.port = *p;
                }
                if static_dir.is_some() {
                    config.static_dir = static_dir.clone();
                }

                // Set up tracing
                tracing_subscriber::fmt::init();

                // PID file for singleton enforcement
                let pid_file_path = midtown::paths::midtown_base_dir().join("webserver.pid");

                if !foreground {
                    use daemonize::Daemonize;

                    // Check if webserver is already running
                    if pid_file_path.exists() {
                        use fs2::FileExt;
                        if let Ok(f) = std::fs::OpenOptions::new().read(true).open(&pid_file_path) {
                            if f.try_lock_exclusive().is_err() {
                                let pid_msg = std::fs::read_to_string(&pid_file_path)
                                    .ok()
                                    .and_then(|s| s.trim().parse::<u32>().ok())
                                    .map(|pid| format!(" (PID {})", pid))
                                    .unwrap_or_default();
                                eprintln!(
                                    "Error: Webserver is already running{}. Stop it first.",
                                    pid_msg
                                );
                                std::process::exit(1);
                            }
                            let _ = f.unlock();
                        }
                    }

                    // Create log directory
                    let log_dir = midtown::paths::midtown_base_dir().join("logs");
                    if let Err(e) = std::fs::create_dir_all(&log_dir) {
                        eprintln!("Failed to create log directory: {}", e);
                        std::process::exit(1);
                    }

                    let stdout = match std::fs::File::options()
                        .append(true)
                        .create(true)
                        .open(log_dir.join("webserver.out"))
                    {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("Failed to open stdout log: {}", e);
                            std::process::exit(1);
                        }
                    };
                    let stderr = match std::fs::File::options()
                        .append(true)
                        .create(true)
                        .open(log_dir.join("webserver.err"))
                    {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("Failed to open stderr log: {}", e);
                            std::process::exit(1);
                        }
                    };

                    let daemonize = Daemonize::new().stdout(stdout).stderr(stderr);

                    match daemonize.start() {
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("Failed to daemonize: {}", e);
                            std::process::exit(1);
                        }
                    }
                }

                // Write PID file with lock
                let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
                rt.block_on(async {
                    // Write PID file
                    if let Err(e) = std::fs::create_dir_all(pid_file_path.parent().unwrap()) {
                        eprintln!("Failed to create PID file directory: {}", e);
                        std::process::exit(1);
                    }
                    {
                        use fs2::FileExt;
                        let pid_file = std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&pid_file_path)
                            .expect("Failed to open PID file");
                        pid_file
                            .try_lock_exclusive()
                            .expect("Failed to lock PID file");
                        use std::io::Write;
                        writeln!(&pid_file, "{}", std::process::id())
                            .expect("Failed to write PID file");
                        // Keep the file handle alive by leaking it (lock is held for process lifetime)
                        std::mem::forget(pid_file);
                    }

                    if let Err(e) = midtown::webserver::run(config).await {
                        eprintln!("Webserver error: {}", e);
                        std::process::exit(1);
                    }
                });
                return;
            }
        }
    }

    // Claude command (exec into claude with auth profile — replaces this process)
    if let Commands::Claude { args } = &command {
        use std::os::unix::process::CommandExt;

        // Use project-aware resolution when inside a project
        let project = midtown::paths::detect_repo_name().unwrap_or_default();

        // Determine which provider to use for this project
        let provider = if project.is_empty() {
            // No project detected - use Claude as default
            midtown::auth::AuthProvider::Claude
        } else if let Some(config) = midtown::config::FullProjectConfig::load(&project) {
            // Check if project has a ZAI profile configured
            if config
                .project
                .auth_profiles
                .as_ref()
                .is_some_and(|m| m.contains_key("zai"))
            {
                midtown::auth::AuthProvider::Zai
            } else {
                midtown::auth::AuthProvider::Claude
            }
        } else {
            midtown::auth::AuthProvider::Claude
        };

        let (profile, profile_dir) = if project.is_empty() {
            (
                midtown::auth::current_profile_for(provider),
                midtown::auth::current_profile_dir_for(provider),
            )
        } else {
            let p = midtown::auth::active_profile_for_project_with_provider(&project, provider);
            let d = midtown::auth::profile_dir_for(provider, &p);
            (p, d)
        };

        // Ensure the profile directory exists
        if !profile_dir.exists() {
            eprintln!(
                "Error: Auth profile '{}' for {} has no config directory at {}",
                profile,
                provider,
                profile_dir.display()
            );
            eprintln!(
                "Run `midtown auth --provider {} login {}` first.",
                provider, profile
            );
            std::process::exit(1);
        }

        let mut cmd = std::process::Command::new("claude");

        // Set provider-specific environment variables
        match provider {
            midtown::auth::AuthProvider::Zai => {
                // z.ai uses ANTHROPIC_AUTH_TOKEN and ANTHROPIC_BASE_URL
                match midtown::launch::zai_env_vars(&profile_dir) {
                    Ok((api_key, base_url)) => {
                        cmd.env("ANTHROPIC_AUTH_TOKEN", &api_key);
                        cmd.env("ANTHROPIC_BASE_URL", &base_url);
                        // Set default model mappings for z.ai (GLM models)
                        cmd.env("ANTHROPIC_DEFAULT_HAIKU_MODEL", "GLM-4.5-Air");
                        cmd.env("ANTHROPIC_DEFAULT_OPUS_MODEL", "GLM-5");
                        cmd.env("ANTHROPIC_DEFAULT_SONNET_MODEL", "GLM-4.7");
                    }
                    Err(e) => {
                        eprintln!("Error: Failed to load z.ai credentials: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            midtown::auth::AuthProvider::Codex => {
                cmd.env("CODEX_HOME", &profile_dir);
            }
            midtown::auth::AuthProvider::Claude => {
                cmd.env("CLAUDE_CONFIG_DIR", &profile_dir);
            }
        }

        cmd.args(args);

        let err = cmd.exec();
        eprintln!("Failed to exec claude: {}", err);
        std::process::exit(1);
    }

    // Task list/view commands (no daemon required — reads from disk)
    if let Commands::Task { command } = &command
        && let Some(result) = cli::handle_task_local(command)
    {
        handle_result(format, result);
        return;
    }

    // All other commands require daemon connection
    let client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to connect to midtown daemon: {}", e);
            eprintln!("Is the daemon running? Try: midtown");
            std::process::exit(1);
        }
    };

    let result = match &command {
        Commands::Channel { command } => cli::handle_channel(command, &client),
        Commands::Coworker { command } => cli::handle_coworker(command, &client),
        Commands::Session { command } => cli::handle_session(command, &client),
        Commands::Task { command } => cli::handle_task(command, &client),
        Commands::Status => cli::handle_status(&client),
        Commands::HeadedWrapper { command } => cli::handle_headed_wrapper(command, &client),
        Commands::Pr { command } => cli::handle_pr(command, &client),
        Commands::Oneshot {
            prompt,
            model,
            system_prompt,
            json_schema,
            max_budget_usd,
            allow_tools,
        } => cli::handle_oneshot(
            &client,
            prompt,
            model,
            system_prompt,
            json_schema.as_deref(),
            *max_budget_usd,
            *allow_tools,
        ),
        // These are handled before daemon connection, so unreachable
        Commands::Daemon { .. }
        | Commands::Start { .. }
        | Commands::Stop { .. }
        | Commands::Restart { .. }
        | Commands::View { .. }
        | Commands::Lead { .. }
        | Commands::Project { .. }
        | Commands::Config { .. }
        | Commands::Chat
        | Commands::E2e { .. }
        | Commands::Auth { .. }
        | Commands::Claude { .. }
        | Commands::State { .. }
        | Commands::Hook { .. }
        | Commands::Diagram { .. }
        | Commands::Log { .. }
        | Commands::Webserver { .. } => unreachable!(),
    };

    handle_result(format, result);
}

fn handle_result(format: OutputFormat, result: Result<cli::Response, String>) {
    match result {
        Ok(response) => {
            let output = match format {
                OutputFormat::Json => response.to_json(),
                OutputFormat::Pretty => response.to_pretty(),
            };
            println!("{}", output);
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
