//! Wake reasons for session nudging.
//!
//! `WakeReason` captures why a session is being woken — used by the unified
//! nudge effects (`NudgeChannelLead`, `NudgeSession`) to format messages
//! for running sessions and initial prompts for fresh spawns.

/// Why a session is being woken up.
#[derive(Debug, Clone)]
pub enum WakeReason {
    // ── Channel lead triggers ──────────────────────────────────────────
    /// A task was created in the channel.
    TaskCreated { task_id: String, subject: String },
    /// A user posted a message in the channel.
    UserMessage { content: String, msg_id: String },
    /// An insight was posted in the channel.
    InsightPosted {
        insight: String,
        agent: String,
        msg_id: String,
        task_id: Option<String>,
        channel_name: String,
    },

    // ── Coworker triggers ──────────────────────────────────────────────
    /// A task was assigned to this coworker (fresh spawn).
    TaskAssigned { task_id: String, subject: String },
    /// A task was claimed by an already-running coworker.
    TaskClaimed { task_id: String, subject: String },
    /// Session recovery after crash/restart.
    SessionRecovery { task_id: String, subject: String },
    /// Review assigned.
    ReviewAssigned { pr_number: u64 },

    // ── Universal ──────────────────────────────────────────────────────
    /// @mention routing.
    Mention {
        from: String,
        content: String,
        msg_id: String,
    },
    /// Generic nudge (freeform message).
    Nudge { message: String },

    // ── DM triggers ────────────────────────────────────────────────────
    /// A direct message was sent to this coworker via their dm-<name> channel.
    DmFromUser {
        content: String,
        msg_id: String,
        /// The coworker's own name, used to format the reply instruction.
        coworker_name: String,
    },
}

impl WakeReason {
    /// Format as a nudge message for an already-running session.
    pub fn to_nudge_message(&self) -> String {
        match self {
            Self::TaskCreated { task_id, subject } => {
                let footer = crate::agents::task_footer(task_id);
                format!(
                    "A task was created in your channel:\n  Task !{task_id}: {subject}\n\n\
                     {footer}"
                )
            }
            Self::UserMessage { content, msg_id } => {
                format!("user ({msg_id}): {content}")
            }
            Self::InsightPosted {
                insight,
                agent,
                msg_id,
                task_id,
                channel_name,
            } => {
                let header = if let Some(tid) = task_id {
                    format!("{agent} working on !{tid} posted an insight in #{channel_name}:")
                } else {
                    format!("{agent} posted an insight in #{channel_name}:")
                };
                format!(
                    "{header}\n\n{insight}\n\n\
                     ONLY reply in the thread if you can add genuine value — additional context, \
                     a correction, or a connection to prior work. Do NOT reply just to acknowledge.\n\n\
                     To reply in the thread:\n  \
                     midtown channel post \"...\" --thread {msg_id} --channel {channel_name}"
                )
            }
            Self::TaskAssigned { task_id, subject } => {
                let footer = crate::agents::task_footer(task_id);
                format!(
                    "You've been assigned task !{task_id}: {subject}. Get started!\n\n\
                     {footer}"
                )
            }
            Self::TaskClaimed { task_id, subject } => {
                let footer = crate::agents::task_footer(task_id);
                format!(
                    "You've been assigned task !{task_id}: {subject}. \
                     Run `midtown task claim {task_id}` to claim it, then get started!\n\n\
                     {footer}"
                )
            }
            Self::SessionRecovery { task_id, subject } => {
                let footer = crate::agents::task_footer(task_id);
                format!(
                    "You've been assigned task !{task_id}: {subject}. \
                     Your previous session was interrupted but your worktree and branch are still intact. \
                     Check your git status and get started!\n\n\
                     {footer}"
                )
            }
            Self::ReviewAssigned { pr_number } => {
                format!("You've been assigned to review PR #{pr_number}. Get started!")
            }
            Self::Mention {
                from,
                content,
                msg_id,
            } => {
                format!("{from} mentioned you ({msg_id}): {content}")
            }
            Self::Nudge { message } => message.clone(),
            Self::DmFromUser {
                content,
                msg_id,
                coworker_name,
            } => {
                format!(
                    "user ({msg_id}): {content}\n\n\
                     Reply with: midtown channel post \"...\" --channel dm-{coworker_name}"
                )
            }
        }
    }

    /// Format as an initial prompt for a fresh channel lead spawn.
    pub fn to_initial_prompt(&self, channel_name: &str) -> String {
        let trigger_section = match self {
            Self::TaskCreated { task_id, subject } => {
                format!(
                    "## Wake trigger\nA task was created in your channel:\n  \
                     Task !{task_id}: {subject}\n\n\
                     ## First Actions\n\
                     1. Read the task details: `midtown task view {task_id}`\n\
                     2. Check recent messages in #{channel_name} for related context\n\n\
                     Reply with: `midtown channel post \"...\" --task {task_id}`"
                )
            }
            Self::UserMessage { content, .. } => {
                format!(
                    "## Wake trigger\nA user posted in your channel:\n  \
                     {content}\n\n\
                     ## First Actions\n\
                     1. Read recent messages in #{channel_name} for context\n\
                     2. Respond to the user's message"
                )
            }
            Self::InsightPosted {
                insight,
                agent,
                task_id,
                msg_id,
                channel_name: insight_channel,
                ..
            } => {
                let header = if let Some(tid) = task_id {
                    format!("{agent} working on !{tid} posted an insight in #{insight_channel}:")
                } else {
                    format!("{agent} posted an insight in #{insight_channel}:")
                };
                format!(
                    "## Wake trigger\n{header}\n  {insight}\n\n\
                     ## First Actions\n\
                     1. Read recent messages in #{channel_name} for context\n\
                     2. ONLY reply in the thread if you can add genuine value — additional context, \
                     a correction, or a connection to prior work. Do NOT reply just to acknowledge.\n   \
                     midtown channel post \"...\" --thread {msg_id} --channel {insight_channel}"
                )
            }
            _ => {
                format!(
                    "## First Actions\n\
                     1. Read recent messages in #{channel_name} for context"
                )
            }
        };

        format!(
            "## Role\nChannel lead for #{channel_name}\n\n\
             ## Channel\n#{channel_name}\n\n\
             ## Mission\n\
             Domain expert for this channel. Track active work, brainstorm, surface issues proactively.\n\n\
             {trigger_section}"
        )
    }
}

#[path = "wake_reason_tests.rs"]
#[cfg(test)]
mod tests;
