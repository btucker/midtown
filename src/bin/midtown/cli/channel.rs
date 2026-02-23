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
        /// Reply in a thread (specify parent message ID)
        #[arg(long = "thread")]
        thread_parent_id: Option<String>,
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
        /// Channel to read from (defaults to MIDTOWN_CHANNEL env var or main channel)
        #[arg(long)]
        channel: Option<String>,
    },
    /// Create a new channel
    Create {
        /// Name of the channel to create
        name: String,
    },
    /// Archive a channel
    Archive {
        /// Name of the channel to archive
        name: String,
    },
    /// Unarchive a channel
    Unarchive {
        /// Name of the channel to restore
        name: String,
    },
    /// Rename a channel
    Rename {
        /// Current channel name
        old: String,
        /// New channel name
        new: String,
    },
}

pub fn handle(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        ChannelCommand::Post {
            message,
            channel,
            thread_parent_id,
        } => match thread_parent_id {
            Some(parent_id) => {
                client.channel_post_in_thread(message, channel.as_deref(), parent_id)
            }
            None => client.channel_post(message, channel.as_deref()),
        },
        ChannelCommand::Read {
            all,
            last,
            since,
            channel,
        } => client.channel_read(*all, last.as_ref(), since.as_deref(), channel.as_deref()),
        ChannelCommand::Create { name } => client.channel_create(name),
        ChannelCommand::Archive { name } => client.channel_archive(name),
        ChannelCommand::Unarchive { name } => client.channel_unarchive(name),
        ChannelCommand::Rename { old, new } => client.channel_rename(old, new),
    }
}
