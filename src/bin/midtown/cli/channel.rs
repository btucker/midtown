use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum ChannelCommand {
    /// Post a message to the channel
    Post {
        /// Message to post
        message: String,
    },
    /// Read messages from the channel
    Read {
        /// Show all messages (not just recent)
        #[arg(long)]
        all: bool,
    },
}

pub fn handle(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        ChannelCommand::Post { message } => {
            // Get sender from MIDTOWN_AGENT env var (set for coworkers)
            let from = std::env::var("MIDTOWN_AGENT").ok();
            client.channel_post(message, from.as_deref())
        }
        ChannelCommand::Read { all } => client.channel_read(*all),
    }
}
