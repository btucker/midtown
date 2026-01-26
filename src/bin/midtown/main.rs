//! Midtown CLI - command-line interface for the midtown daemon.
//!
//! This binary provides user-facing commands for interacting with the daemon,
//! managing channels, coworkers, tasks, and pull requests.

use clap::{Parser, Subcommand};

mod cli;
mod client;

use cli::{ChannelCommand, CoworkerCommand, PrCommand, TaskCommand};
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

#[derive(Subcommand, Clone)]
enum Commands {
    /// Run the daemon server (internal - use 'start' instead)
    #[command(hide = true)]
    Daemon {
        /// Path to the Unix socket
        #[arg(short, long)]
        socket: Option<std::path::PathBuf>,

        /// Working directory for spawned coworkers
        #[arg(short, long)]
        workdir: Option<std::path::PathBuf>,

        /// Enable verbose logging
        #[arg(short, long)]
        verbose: bool,

        /// Port for GitHub webhook server (disabled if not set)
        #[arg(long)]
        webhook_port: Option<u16>,
    },
    /// Start midtown (daemon + tmux session)
    Start {
        /// Start daemon only, without tmux session
        #[arg(long)]
        daemon_only: bool,
    },
    /// Stop midtown (daemon + tmux session)
    Stop {
        /// Keep the tmux session running
        #[arg(long)]
        keep_session: bool,
    },
    /// Restart midtown (stop + start)
    Restart,
    /// Attach to the project's tmux session
    Attach,
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
    /// Task management commands
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// Show system status
    Status,
    /// Pull request commands
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Lead-specific commands
    Lead {
        #[command(subcommand)]
        command: LeadCommand,
    },
    /// Open IRC-style chat TUI
    Chat,
}

#[derive(Subcommand, Clone)]
enum LeadCommand {
    /// Register this session for task sharing with coworkers
    RegisterSession,
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
    let command = cli
        .command
        .unwrap_or(Commands::Start { daemon_only: false });

    // Handle commands that don't require daemon connection
    if let Commands::Task {
        command: TaskCommand::Hook { event },
    } = &command
    {
        let result = cli::handle_task_hook(event);
        handle_result(format, result);
        return;
    }

    // Stop hook also works standalone (no daemon required)
    if let Commands::Coworker {
        command: CoworkerCommand::StopHook,
    } = &command
    {
        let result = cli::handle_coworker_stop_hook();
        handle_result(format, result);
        return;
    }

    // Link tasks also works standalone (SessionStart hook)
    if let Commands::Coworker {
        command: CoworkerCommand::LinkTasks,
    } = &command
    {
        let result = cli::handle_coworker_link_tasks();
        handle_result(format, result);
        return;
    }

    // Daemon command (runs the daemon server - internal use)
    if let Commands::Daemon {
        socket,
        workdir,
        verbose,
        webhook_port,
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

        // Run the daemon (this blocks until shutdown)
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        if let Err(e) = rt.block_on(midtown::daemon::run(config)) {
            eprintln!("Daemon error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // Start command (starts daemon, doesn't need existing connection)
    if let Commands::Start { daemon_only } = &command {
        let result = cli::handle_start(*daemon_only);
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
    if let Commands::Restart = &command {
        let result = cli::handle_restart();
        handle_result(format, result);
        return;
    }

    // Attach command (just tmux, doesn't need daemon)
    if let Commands::Attach = &command {
        let result = cli::handle_attach();
        handle_result(format, result);
        return;
    }

    // Lead commands (no daemon required)
    if let Commands::Lead { command } = &command {
        let result = match command {
            LeadCommand::RegisterSession => cli::handle_register_session(),
        };
        handle_result(format, result);
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
        Commands::Task { command } => cli::handle_task(command, &client),
        Commands::Status => cli::handle_status(&client),
        Commands::Pr { command } => cli::handle_pr(command, &client),
        // These are handled before daemon connection, so unreachable
        Commands::Daemon { .. }
        | Commands::Start { .. }
        | Commands::Stop { .. }
        | Commands::Restart
        | Commands::Attach
        | Commands::Lead { .. }
        | Commands::Chat => unreachable!(),
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
