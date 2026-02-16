use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum ChannelCommand {
    /// Post a message to the channel
    Post {
        /// Message to post
        message: String,
        /// Channel to post to (defaults to main channel if not specified)
        #[arg(long)]
        channel: Option<String>,
    },
    /// Read messages from the channel
    Read {
        /// Show all messages (not just recent)
        #[arg(long)]
        all: bool,
        /// Show only the last N messages
        #[arg(long)]
        last: Option<usize>,
        /// Show messages from the last duration (e.g., 5m, 1h, 30s)
        #[arg(long)]
        since: Option<String>,
    },
}

pub fn handle(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        ChannelCommand::Post { message, channel } => {
            client.channel_post(message, channel.as_deref())
        }
        ChannelCommand::Read { all, last, since } => {
            client.channel_read(*all, last.as_ref(), since.as_deref())
        }
    }
}
