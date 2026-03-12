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
        #[arg(long = "thread", conflicts_with = "task")]
        thread_parent_id: Option<String>,
        /// Auto-thread to the task's announcement message (specify task ID)
        #[arg(long = "task", conflicts_with = "thread_parent_id")]
        task: Option<String>,
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
        /// Read only messages in a thread (specify parent message ID)
        #[arg(long = "thread")]
        thread_parent_id: Option<String>,
        /// Read a specific message by its UUID
        #[arg(long)]
        message: Option<String>,
        /// Return N messages before and after the target message (requires --message)
        #[arg(short = 'C', long, requires = "message")]
        context: Option<usize>,
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

#[path = "channel_post_tests.rs"]
#[cfg(test)]
mod tests;

pub fn handle(cmd: &ChannelCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        ChannelCommand::Post {
            message,
            channel,
            thread_parent_id,
            task,
        } => {
            if let Some(task_id) = task {
                client.channel_post_for_task(message, channel.as_deref(), task_id)
            } else if let Some(parent_id) = thread_parent_id {
                client.channel_post_in_thread(message, channel.as_deref(), parent_id)
            } else if let Ok(env_task_id) = std::env::var("MIDTOWN_TASK_ID") {
                // Auto-thread to the task when MIDTOWN_TASK_ID env var is set
                // (injected by the daemon at spawn time via LaunchConfig.task_id).
                // Fall back to regular post if the task no longer exists (stale ID).
                client
                    .channel_post_for_task(message, channel.as_deref(), &env_task_id)
                    .or_else(|err| {
                        eprintln!(
                            "warning: auto-threading to task !{} failed ({}), falling back to regular post",
                            env_task_id, err
                        );
                        client.channel_post(message, channel.as_deref())
                    })
            } else {
                client.channel_post(message, channel.as_deref())
            }
        }
        ChannelCommand::Read {
            all,
            last,
            since,
            channel,
            thread_parent_id,
            message,
            context,
        } => client.channel_read(
            *all,
            last.as_ref(),
            since.as_deref(),
            channel.as_deref(),
            thread_parent_id.as_deref(),
            message.as_deref(),
            *context,
        ),
        ChannelCommand::Create { name } => client.channel_create(name),
        ChannelCommand::Archive { name } => client.channel_archive(name),
        ChannelCommand::Unarchive { name } => client.channel_unarchive(name),
        ChannelCommand::Rename { old, new } => client.channel_rename(old, new),
    }
}
