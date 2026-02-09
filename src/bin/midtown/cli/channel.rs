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
    },
}

pub fn handle(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        ChannelCommand::Post { message, channel } => {
            client.channel_post(message, channel.as_deref())
        }
        ChannelCommand::Read { all } => client.channel_read(*all),
    }
}
