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

    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum OutputFormat {
    Json,
    Pretty,
}

#[derive(Subcommand)]
enum Commands {
    /// Start midtown (daemon + Lead session)
    Start {
        /// Start daemon only, without Lead session
        #[arg(long)]
        daemon_only: bool,
    },
    /// Stop midtown (daemon + Lead session)
    Stop {
        /// Keep the Lead session running
        #[arg(long)]
        keep_lead: bool,
    },
    /// Attach to the Lead session (shortcut for tmux attach -t midtown-lead)
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
}

fn main() {
    let cli = Cli::parse();

    // Handle commands that don't require daemon connection
    if let Commands::Task { command: TaskCommand::Hook { event } } = &cli.command {
        let result = cli::handle_task_hook(event);
        handle_result(&cli, result);
        return;
    }

    // Stop hook also works standalone (no daemon required)
    if let Commands::Coworker { command: CoworkerCommand::StopHook } = &cli.command {
        let result = cli::handle_coworker_stop_hook();
        handle_result(&cli, result);
        return;
    }

    // Start command (starts daemon, doesn't need existing connection)
    if let Commands::Start { daemon_only } = &cli.command {
        let result = cli::handle_start(*daemon_only);
        handle_result(&cli, result);
        return;
    }

    // Stop command (stops daemon, doesn't need existing connection)
    if let Commands::Stop { keep_lead } = &cli.command {
        let result = cli::handle_stop(*keep_lead);
        handle_result(&cli, result);
        return;
    }

    // Attach command (just tmux, doesn't need daemon)
    if let Commands::Attach = &cli.command {
        let result = cli::handle_attach();
        handle_result(&cli, result);
        return;
    }

    // All other commands require daemon connection
    let client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to connect to midtown daemon: {}", e);
            eprintln!("Is the daemon running? Try: midtown start");
            std::process::exit(1);
        }
    };

    let result = match &cli.command {
        Commands::Channel { command } => cli::handle_channel(command, &client),
        Commands::Coworker { command } => cli::handle_coworker(command, &client),
        Commands::Task { command } => cli::handle_task(command, &client),
        Commands::Status => cli::handle_status(&client),
        Commands::Pr { command } => cli::handle_pr(command, &client),
        // These are handled before daemon connection, so unreachable
        Commands::Start { .. } | Commands::Stop { .. } | Commands::Attach => unreachable!(),
    };

    handle_result(&cli, result);
}

fn handle_result(cli: &Cli, result: Result<cli::Response, String>) {
    match result {
        Ok(response) => {
            let output = match cli.format {
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
