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

    let client = match DaemonClient::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Failed to connect to midtown daemon: {}", e);
            eprintln!("Is the daemon running? Try: midtownd");
            std::process::exit(1);
        }
    };

    let result = match &cli.command {
        Commands::Channel { command } => cli::handle_channel(command, &client),
        Commands::Coworker { command } => cli::handle_coworker(command, &client),
        Commands::Task { command } => cli::handle_task(command, &client),
        Commands::Status => cli::handle_status(&client),
        Commands::Pr { command } => cli::handle_pr(command, &client),
    };

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
