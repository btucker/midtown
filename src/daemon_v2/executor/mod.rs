pub mod channel_io;
pub mod github;
pub mod spawn;
pub mod webhook;

#[path = "nudge_tests.rs"]
#[cfg(test)]
mod nudge_tests;

#[path = "dispatch_tests.rs"]
#[cfg(test)]
mod dispatch_tests;

use std::collections::HashMap;
use std::path::Path;

use crate::daemon_v2::decisions::{Command, SpawnConfig};
use crate::daemon_v2::events::DomainEvent;
use crate::daemon_v2::projections::Projections;
use crate::headless::HeadlessSession;
use crate::paths::ProjectPaths;

/// What to do when executing a NudgeAgent command.
#[derive(Debug)]
pub enum NudgeAction {
    /// Agent is running — deliver the message directly.
    Deliver,
    /// Agent is stopped but has a session_id — resume then deliver.
    ResumeAndDeliver { session_id: String },
    /// Agent is stopped with no session_id — spawn a replacement then deliver.
    RespawnAndDeliver { config: Box<SpawnConfig> },
    /// Agent is unknown — drop the nudge.
    Drop,
}

/// Determine the nudge strategy for a given agent.
/// Per spec 1.4: stopped agents are resumed (with session_id) or respawned (without).
pub fn resolve_nudge_action(agent_id: &str, proj: &Projections) -> NudgeAction {
    let agent = match proj.agents.by_id.get(agent_id) {
        Some(a) => a,
        None => return NudgeAction::Drop,
    };
    if proj.agents.running.contains(agent_id) {
        NudgeAction::Deliver
    } else if let Some(ref session_id) = agent.session_id {
        NudgeAction::ResumeAndDeliver {
            session_id: session_id.clone(),
        }
    } else {
        NudgeAction::RespawnAndDeliver {
            config: Box::new(spawn_config_from_agent(agent)),
        }
    }
}

/// Result sent from background tasks back to the main event loop.
pub enum ExecutorResult {
    /// Events to apply to store + projections + broadcast.
    /// `lifecycle_key`: if set, clear this key from the lifecycle guard (for failed spawns).
    Events {
        events: Vec<DomainEvent>,
        lifecycle_key: Option<String>,
    },
    /// A new session is ready — main loop inserts into sessions map.
    /// `lifecycle_key`: if set, clear this key from the lifecycle guard instead of `id`.
    SessionReady {
        id: String,
        session: Box<HeadlessSession>,
        events: Vec<DomainEvent>,
        lifecycle_key: Option<String>,
    },
    /// A lifecycle operation (stop) completed — deliver stashed nudges.
    LifecycleComplete {
        id: String,
        events: Vec<DomainEvent>,
    },
}

/// Classification of how a command should be executed.
pub enum CommandClass {
    /// Execute immediately in the main loop (fast, may need &mut sessions).
    Inline,
    /// Execute in a background tokio task (slow I/O).
    Background,
    /// Needs runtime resolution (NudgeAgent — may be inline or background).
    NeedsResolution,
}

/// Classify a command as inline, background, or needs-resolution.
pub fn classify_command(cmd: &Command) -> CommandClass {
    match cmd {
        // Inline: pure event emission or fast session ops
        Command::AssignTask { .. }
        | Command::CompleteTask { .. }
        | Command::ResetTask { .. }
        | Command::GarbageCollect { .. }
        | Command::CreateWorktree { .. }
        | Command::RemoveWorktree { .. }
        | Command::Post { .. }
        | Command::PostSystem { .. }
        | Command::PollProcessHealth
        | Command::PersistEvents(_) => CommandClass::Inline,

        // Background: slow I/O
        Command::SpawnAgent(_)
        | Command::ResumeAgent { .. }
        | Command::StopAgent { .. }
        | Command::PollPrs
        | Command::MergePr { .. }
        | Command::PostPrComment { .. }
        | Command::RerunCi { .. } => CommandClass::Background,

        // Needs resolution at dispatch time
        Command::NudgeAgent { .. } => CommandClass::NeedsResolution,
    }
}

/// Tracks agents with in-flight lifecycle operations (spawn, stop).
/// Nudges arriving during these operations are stashed and delivered when complete.
#[derive(Default)]
pub struct LifecycleGuard {
    pending: HashMap<String, Vec<String>>,
    /// Maps an alias (e.g., new agent ID after respawn) to the original pending key.
    aliases: HashMap<String, String>,
}

impl LifecycleGuard {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Mark an agent as having an in-flight spawn or stop.
    pub fn mark_pending(&mut self, agent_id: String) {
        self.pending.entry(agent_id).or_default();
    }

    /// Check if an agent has an in-flight operation.
    pub fn is_pending(&self, agent_id: &str) -> bool {
        self.pending.contains_key(agent_id)
    }

    /// Stash a nudge message for delivery after the operation completes.
    pub fn stash_nudge(&mut self, agent_id: &str, message: String) {
        if let Some(stashed) = self.pending.get_mut(agent_id) {
            stashed.push(message);
        }
    }

    /// Register an alias so that `complete(alias)` resolves to the original key.
    /// Used when a respawned agent gets a new ID but the guard was keyed on the old ID.
    pub fn add_alias(&mut self, alias: String, original_key: String) {
        self.aliases.insert(alias, original_key);
    }

    /// Complete a lifecycle operation. Returns stashed nudge messages.
    /// Resolves aliases: if `agent_id` is an alias, completes the original key.
    pub fn complete(&mut self, agent_id: &str) -> Vec<String> {
        let resolved = self.aliases.remove(agent_id);
        let key = resolved.as_deref().unwrap_or(agent_id);
        self.pending.remove(key).unwrap_or_default()
    }
}

/// Execute a command that is known to be fast and inline.
/// These commands either emit pure events or need &mut sessions but are non-blocking.
pub fn execute_inline(
    command: Command,
    sessions: &mut HashMap<String, HeadlessSession>,
    channels_dir: &Path,
) -> Vec<DomainEvent> {
    match command {
        Command::PollProcessHealth => {
            let mut events = vec![];
            let mut dead_ids = vec![];
            for (id, session) in sessions.iter_mut() {
                match session.try_wait() {
                    Ok(Some(_)) => {
                        dead_ids.push(id.clone());
                        events.push(DomainEvent::AgentStopped {
                            id: id.clone(),
                            reason: "process exited".into(),
                        });
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%id, %e, "try_wait error"),
                }
            }
            for id in dead_ids {
                sessions.remove(&id);
            }
            events
        }
        Command::AssignTask { task_id, agent_id } => {
            vec![DomainEvent::TaskAssigned { task_id, agent_id }]
        }
        Command::CompleteTask { task_id } => {
            vec![DomainEvent::TaskCompleted { task_id }]
        }
        Command::ResetTask { task_id } => {
            tracing::info!(%task_id, "resetting task to pending");
            vec![DomainEvent::TaskReset {
                task_id,
                reason: "agent died".into(),
            }]
        }
        Command::GarbageCollect { agent_id } => {
            tracing::info!(%agent_id, "garbage collecting agent record");
            vec![DomainEvent::AgentGarbageCollected { id: agent_id }]
        }
        Command::CreateWorktree { task_id, branch } => {
            tracing::debug!(%task_id, %branch, "CreateWorktree (managed by daemon)");
            vec![]
        }
        Command::RemoveWorktree { task_id } => {
            tracing::debug!(%task_id, "RemoveWorktree (managed by daemon)");
            vec![]
        }
        Command::Post {
            channel,
            sender,
            content,
            thread_id,
        } => {
            if let Err(e) = channel_io::post_message(
                channels_dir,
                &channel,
                &sender,
                &content,
                thread_id.as_deref(),
            ) {
                tracing::error!(%e, %channel, "failed to post message");
                return vec![];
            }
            vec![DomainEvent::MessagePosted {
                id: uuid::Uuid::new_v4().to_string(),
                channel,
                sender,
                content,
                thread_id,
                tool_data: None,
                auto_output: false,
            }]
        }
        Command::PostSystem { channel, content } => {
            if let Err(e) = channel_io::post_system_message(channels_dir, &channel, &content) {
                tracing::error!(%e, %channel, "failed to post system message");
                return vec![];
            }
            vec![DomainEvent::MessagePosted {
                id: uuid::Uuid::new_v4().to_string(),
                channel,
                sender: "midtown".into(),
                content,
                thread_id: None,
                tool_data: None,
                auto_output: false,
            }]
        }
        other => {
            tracing::error!(?other, "execute_inline called with non-inline command");
            vec![]
        }
    }
}

// ── Background spawn functions ──────────────────────────────────────

/// Spawn PollPrs in a background task. Results sent via result_tx.
pub fn spawn_background_poll_prs(
    work: crate::daemon_v2::projections::work::WorkIndex,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        if let Some(status) = github::check_rate_limit().await
            && github::should_throttle(&status)
        {
            tracing::warn!(
                remaining = status.remaining,
                "PR polling skipped — rate limit low"
            );
            return;
        }
        let events = match (
            github::fetch_open_prs().await,
            github::fetch_merged_prs().await,
        ) {
            (Ok(open), Ok(merged)) => github::diff_pr_state(&work, &open, &merged),
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(%e, "PR polling failed");
                vec![]
            }
        };
        if !events.is_empty() {
            let _ = result_tx
                .send(ExecutorResult::Events {
                    events,
                    lifecycle_key: None,
                })
                .await;
        }
    });
}

/// Spawn agent in a background task. Session + events sent via result_tx.
pub fn spawn_background_agent(
    config: SpawnConfig,
    paths: ProjectPaths,
    channels_dir: std::path::PathBuf,
    event_tx: tokio::sync::broadcast::Sender<DomainEvent>,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
    lifecycle_key: Option<String>,
) {
    tokio::spawn(async move {
        match spawn::spawn_agent(&config, &paths).await {
            Ok((mut session, events)) => {
                drain_session_output(
                    &mut session,
                    &config.name,
                    config.channel.as_deref(),
                    config.bound_thread_id.as_deref(),
                    &channels_dir,
                    &event_tx,
                    None,
                );
                let id = events
                    .iter()
                    .find_map(|e| match e {
                        DomainEvent::AgentCreated { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                // Auto-create DM channel for agents not bound to a channel/thread
                if config.channel.is_none() && config.bound_thread_id.is_none() {
                    let dm = crate::daemon_v2::decisions::lifecycle::create_dm_channel_name(
                        &config.name,
                    );
                    let _ = channel_io::post_system_message(
                        &channels_dir,
                        &dm,
                        &format!("DM channel for {}", config.name),
                    );
                }

                let _ = result_tx
                    .send(ExecutorResult::SessionReady {
                        id,
                        session: Box::new(session),
                        events,
                        lifecycle_key,
                    })
                    .await;
            }
            Err(e) => {
                tracing::error!(%e, name = %config.name, "failed to spawn agent");
                let _ = result_tx
                    .send(ExecutorResult::Events {
                        events: vec![DomainEvent::AgentSpawnFailed {
                            name: config.name.clone(),
                            agent_type: config.agent_type.clone(),
                            reason: e.to_string(),
                        }],
                        lifecycle_key,
                    })
                    .await;
            }
        }
    });
}

/// Resume agent in a background task.
#[allow(clippy::too_many_arguments)]
pub fn spawn_background_resume(
    agent_id: String,
    agent: crate::daemon_v2::projections::agents::Agent,
    paths: ProjectPaths,
    channels_dir: std::path::PathBuf,
    event_tx: tokio::sync::broadcast::Sender<DomainEvent>,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
    lifecycle_key: Option<String>,
    working_dir: Option<String>,
) {
    tokio::spawn(async move {
        let mut config = spawn_config_from_agent(&agent);
        config.working_dir = working_dir;
        let session_id = match &agent.session_id {
            Some(sid) => sid.clone(),
            None => {
                let _ = result_tx
                    .send(ExecutorResult::Events {
                        events: vec![DomainEvent::AgentSpawnFailed {
                            name: agent.name.clone(),
                            agent_type: agent.agent_type.clone(),
                            reason: "no session_id for resume".into(),
                        }],
                        lifecycle_key,
                    })
                    .await;
                return;
            }
        };
        let launch_config = spawn::build_launch_config(&config, paths.dir_key());
        let mut headless_config = launch_config.to_headless_config(&paths);
        headless_config.resume_session_id = Some(session_id);

        match HeadlessSession::spawn(&headless_config).await {
            Ok(mut session) => {
                let pid = session.pid().unwrap_or(0);
                drain_session_output(
                    &mut session,
                    &agent.name,
                    agent.channel.as_deref(),
                    agent.bound_thread_id.as_deref(),
                    &channels_dir,
                    &event_tx,
                    Some(result_tx.clone()),
                );
                let _ = result_tx
                    .send(ExecutorResult::SessionReady {
                        id: agent_id.clone(),
                        session: Box::new(session),
                        events: vec![DomainEvent::AgentResumed { id: agent_id, pid }],
                        lifecycle_key,
                    })
                    .await;
            }
            Err(e) => {
                let _ = result_tx
                    .send(ExecutorResult::Events {
                        events: vec![DomainEvent::AgentSpawnFailed {
                            name: agent.name.clone(),
                            agent_type: agent.agent_type.clone(),
                            reason: format!("resume failed: {e}"),
                        }],
                        lifecycle_key,
                    })
                    .await;
            }
        }
    });
}

/// Background stop: kill the session process.
pub fn spawn_background_stop(
    id: String,
    reason: String,
    mut session: HeadlessSession,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        if let Err(e) = spawn::stop_agent(&mut session).await {
            tracing::warn!(%id, %e, "error stopping agent");
            let _ = result_tx
                .send(ExecutorResult::LifecycleComplete {
                    id: id.clone(),
                    events: vec![DomainEvent::AgentStopFailed {
                        id,
                        reason: e.to_string(),
                    }],
                })
                .await;
            return;
        }
        let _ = result_tx
            .send(ExecutorResult::LifecycleComplete {
                id: id.clone(),
                events: vec![DomainEvent::AgentStopped { id, reason }],
            })
            .await;
    });
}

/// Background gh CLI command (merge, comment, rerun).
pub fn spawn_background_gh_command(
    command: Command,
    result_tx: tokio::sync::mpsc::Sender<ExecutorResult>,
) {
    tokio::spawn(async move {
        let events = match command {
            Command::MergePr { number } => {
                tracing::info!(%number, "merging PR");
                match tokio::process::Command::new("gh")
                    .args(["pr", "merge", &number.to_string(), "--squash", "--auto"])
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => {
                        vec![DomainEvent::PrMerged {
                            number,
                            branch: String::new(),
                        }]
                    }
                    Ok(output) => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        tracing::error!(%number, %err, "gh pr merge failed");
                        vec![]
                    }
                    Err(e) => {
                        tracing::error!(%number, %e, "gh pr merge failed");
                        vec![]
                    }
                }
            }
            Command::PostPrComment { number, body } => {
                tracing::info!(%number, "posting PR comment");
                match tokio::process::Command::new("gh")
                    .args(["pr", "comment", &number.to_string(), "--body", &body])
                    .output()
                    .await
                {
                    Ok(output) if !output.status.success() => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        tracing::warn!(%number, %err, "gh pr comment failed");
                    }
                    Err(e) => tracing::warn!(%number, %e, "gh pr comment failed"),
                    _ => {}
                }
                vec![]
            }
            Command::RerunCi { run_id } => {
                tracing::info!(%run_id, "rerunning CI");
                match tokio::process::Command::new("gh")
                    .args(["run", "rerun", &run_id.to_string()])
                    .output()
                    .await
                {
                    Ok(output) if !output.status.success() => {
                        let err = String::from_utf8_lossy(&output.stderr);
                        tracing::warn!(%run_id, %err, "gh run rerun failed");
                    }
                    Err(e) => tracing::warn!(%run_id, %e, "gh run rerun failed"),
                    _ => {}
                }
                vec![]
            }
            _ => vec![],
        };
        if !events.is_empty() {
            let _ = result_tx
                .send(ExecutorResult::Events {
                    events,
                    lifecycle_key: None,
                })
                .await;
        }
    });
}

/// Build a SpawnConfig that recreates an agent with the same configuration.
pub fn spawn_config_from_agent(
    agent: &crate::daemon_v2::projections::agents::Agent,
) -> SpawnConfig {
    SpawnConfig {
        name: agent.name.clone(),
        kind: agent.kind.clone(),
        agent_type: agent.agent_type.clone(),
        provider: agent.provider.clone(),
        channel: agent.channel.clone(),
        task_id: agent.task_id.clone(),
        initial_prompt: None,
        working_dir: None,
        model: None,
        bound_thread_id: agent.bound_thread_id.clone(),
        fork_from_session: None,
        icon: agent.icon.clone(),
        color: agent.color.clone(),
    }
}

/// Detach stdout/stderr receivers and spawn a background task that:
/// 1. Drains pipes so the child process doesn't block
/// 2. Extracts assistant text from stream events and posts to the channel
fn drain_session_output(
    session: &mut HeadlessSession,
    agent_name: &str,
    channel: Option<&str>,
    bound_thread_id: Option<&str>,
    channels_dir: &Path,
    event_tx: &tokio::sync::broadcast::Sender<DomainEvent>,
    result_tx: Option<tokio::sync::mpsc::Sender<ExecutorResult>>,
) {
    if let Some((mut stdout_rx, mut stderr_rx)) = session.take_receivers() {
        let name = agent_name.to_string();
        let channel = channel.map(|s| s.to_string());
        let thread_id = bound_thread_id.map(|s| s.to_string());
        let channels_dir = channels_dir.to_path_buf();
        let event_tx = event_tx.clone();
        let result_tx_opt = result_tx;
        tokio::spawn(async move {
            use crate::headless::StreamEvent;

            let mut pending_events: Vec<StreamEvent> = Vec::new();
            // Spec 4.1: flush on turn completion (Result event) for low latency,
            // with a 2-second fallback timer for long-running turns.
            let mut flush_interval = tokio::time::interval(std::time::Duration::from_secs(2));
            flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    msg = stdout_rx.recv() => {
                        match msg {
                            Some(event) => {
                                // Log error results so session failures appear in daemon.log
                                if let StreamEvent::Result { is_error: true, ref extra, .. } = event {
                                    let errors = extra.get("errors");
                                    tracing::warn!(agent = %name, ?errors, "session exited with error");

                                    // Detect "No conversation found" so the daemon clears the
                                    // stale session_id and spawns fresh on next retry.
                                    if let Some(arr) = errors.and_then(|e| e.as_array()) {
                                        let is_not_found = arr.iter().any(|e| {
                                            e.as_str()
                                                .is_some_and(|s| s.contains("No conversation found"))
                                        });
                                        if is_not_found && let Some(ref tx) = result_tx_opt {
                                            let _ = tx
                                                .send(ExecutorResult::Events {
                                                    events: vec![
                                                        DomainEvent::AgentSessionNotFound {
                                                            name: name.clone(),
                                                        },
                                                    ],
                                                    lifecycle_key: None,
                                                })
                                                .await;
                                        }
                                    }
                                }
                                // Flush immediately on turn completion (Result event)
                                let is_turn_end = matches!(&event, StreamEvent::Result { .. });
                                pending_events.push(event);
                                if is_turn_end {
                                    flush_auto_output(&name, &channel, thread_id.as_deref(), &channels_dir, &mut pending_events, &event_tx);
                                }
                            }
                            None => {
                                // Stream ended — flush remaining
                                flush_auto_output(&name, &channel, thread_id.as_deref(), &channels_dir, &mut pending_events, &event_tx);
                                break;
                            }
                        }
                    }
                    msg = stderr_rx.recv() => {
                        match msg {
                            Some(line) if !line.trim().is_empty() => {
                                tracing::warn!(agent = %name, "stderr: {line}");
                            }
                            None => break,
                            _ => {}
                        }
                    }
                    _ = flush_interval.tick() => {
                        flush_auto_output(&name, &channel, thread_id.as_deref(), &channels_dir, &mut pending_events, &event_tx);
                    }
                }
            }
            tracing::debug!(agent = %name, "stdout/stderr drain ended");
        });
    }
}

/// Extract assistant text and tool data from accumulated stream events and
/// post to channel. Also extracts `★ Insight` blocks and posts them as
/// standalone messages so they remain visible regardless of the "Show full
/// lead output" setting.
fn flush_auto_output(
    agent_name: &str,
    channel: &Option<String>,
    thread_id: Option<&str>,
    channels_dir: &std::path::Path,
    events: &mut Vec<crate::headless::StreamEvent>,
    event_tx: &tokio::sync::broadcast::Sender<DomainEvent>,
) {
    if events.is_empty() {
        return;
    }
    let text = crate::stream::extract_assistant_text(events)
        .trim()
        .to_string();
    let tool_blocks = crate::stream::extract_tool_blocks(events);
    events.clear();

    if text.is_empty() && tool_blocks.is_empty() {
        return;
    }
    if let Some(ch) = channel {
        // Extract and post insights as standalone (non-auto-output) messages
        for insight in crate::stream::extract_insights(&text) {
            let insight_content = format!("💡 {insight}");
            match channel_io::post_message(channels_dir, ch, agent_name, &insight_content, None) {
                Ok(id) => {
                    let _ = event_tx.send(DomainEvent::MessagePosted {
                        id,
                        channel: ch.clone(),
                        sender: agent_name.to_string(),
                        content: insight_content,
                        thread_id: None,
                        tool_data: None,
                        auto_output: false, // insights are always visible
                    });
                }
                Err(e) => {
                    tracing::warn!(agent = %agent_name, %ch, %e, "failed to post insight");
                }
            }
        }

        // Post assistant text + tool data as a single auto-output message.
        // Tool blocks are attached to the text message rather than posted
        // separately, preventing empty message bubbles in the web UI.
        let thread_owned = thread_id.map(String::from);
        let tool_data = if tool_blocks.is_empty() {
            None
        } else {
            Some(tool_blocks.clone())
        };

        // Skip entirely empty turns (no text, no tool data)
        if text.is_empty() && tool_data.is_none() {
            return;
        }

        let msg_id = match channel_io::post_auto_output(
            channels_dir,
            ch,
            agent_name,
            &text,
            if tool_blocks.is_empty() {
                None
            } else {
                Some(tool_blocks)
            },
            thread_id,
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(agent = %agent_name, %ch, %e, "failed to auto-post output");
                return;
            }
        };
        let _ = event_tx.send(DomainEvent::MessagePosted {
            id: msg_id,
            channel: ch.clone(),
            sender: agent_name.to_string(),
            content: text,
            thread_id: thread_owned,
            tool_data,
            auto_output: true,
        });
    }
}
