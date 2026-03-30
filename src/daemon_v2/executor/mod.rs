pub mod channel_io;
pub mod github;
pub mod spawn;
pub mod webhook;

#[path = "nudge_tests.rs"]
#[cfg(test)]
mod nudge_tests;

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

/// Build a SpawnConfig that recreates an agent with the same configuration.
fn spawn_config_from_agent(agent: &crate::daemon_v2::projections::agents::Agent) -> SpawnConfig {
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

pub async fn execute(
    command: Command,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
    projections: &Projections,
    channels_dir: &Path,
) -> Vec<DomainEvent> {
    match command {
        Command::SpawnAgent(config) => execute_spawn(&config, sessions, paths, channels_dir).await,
        Command::StopAgent { id, reason } => {
            if let Some(mut session) = sessions.remove(&id)
                && let Err(e) = spawn::stop_agent(&mut session).await
            {
                tracing::warn!(%id, %e, "error stopping agent");
                return vec![DomainEvent::AgentStopFailed {
                    id,
                    reason: e.to_string(),
                }];
            }
            vec![DomainEvent::AgentStopped { id, reason }]
        }
        Command::PollProcessHealth => {
            let mut events = vec![];
            let mut dead_ids = vec![];

            for (id, session) in sessions.iter_mut() {
                match session.try_wait() {
                    Ok(Some(_exit_status)) => {
                        dead_ids.push(id.clone());
                        events.push(DomainEvent::AgentStopped {
                            id: id.clone(),
                            reason: "process exited".into(),
                        });
                    }
                    Ok(None) => { /* still running */ }
                    Err(e) => {
                        tracing::warn!(%id, %e, "try_wait error");
                    }
                }
            }

            for id in dead_ids {
                sessions.remove(&id);
            }

            events
        }
        Command::ResetTask { task_id } => {
            tracing::info!(%task_id, "resetting task to pending");
            vec![DomainEvent::TaskReset {
                task_id,
                reason: "agent died".into(),
            }]
        }
        Command::PollPrs => {
            match (
                github::fetch_open_prs().await,
                github::fetch_merged_prs().await,
            ) {
                (Ok(open), Ok(merged)) => github::diff_pr_state(&projections.work, &open, &merged),
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!(%e, "PR polling failed");
                    vec![]
                }
            }
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
            }]
        }
        Command::NudgeAgent { id, message } => {
            execute_nudge(&id, &message, projections, sessions, paths, channels_dir).await
        }
        Command::ResumeAgent { id } => execute_resume(&id, projections, sessions, paths).await,
        Command::AssignTask { task_id, agent_id } => {
            vec![DomainEvent::TaskAssigned { task_id, agent_id }]
        }
        Command::CompleteTask { task_id } => {
            vec![DomainEvent::TaskCompleted { task_id }]
        }
        Command::CreateWorktree { task_id, branch } => {
            tracing::debug!(%task_id, %branch, "CreateWorktree command received (worktree lifecycle managed by daemon)");
            vec![]
        }
        Command::RemoveWorktree { task_id } => {
            tracing::debug!(%task_id, "RemoveWorktree command received (worktree lifecycle managed by daemon)");
            vec![]
        }
        Command::GarbageCollect { agent_id } => {
            tracing::info!(%agent_id, "garbage collecting agent record");
            vec![DomainEvent::AgentGarbageCollected { id: agent_id }]
        }
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
    }
}

// ── Shared execution helpers ─────────────────────────────────────────

/// Spawn an agent, drain its stdout/stderr, insert into sessions map,
/// and create a DM channel if needed. Single path for all spawning.
async fn execute_spawn(
    config: &SpawnConfig,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
    channels_dir: &Path,
) -> Vec<DomainEvent> {
    match spawn::spawn_agent(config, paths).await {
        Ok((mut session, events)) => {
            drain_session_output(&mut session, &config.name);
            if let Some(DomainEvent::AgentCreated { id, .. }) = events.first() {
                sessions.insert(id.clone(), session);
            }
            // Auto-create DM channel for agents whose output isn't bound to a channel/thread
            if config.channel.is_none() && config.bound_thread_id.is_none() {
                let dm_channel =
                    crate::daemon_v2::decisions::lifecycle::create_dm_channel_name(&config.name);
                if let Err(e) = channel_io::post_system_message(
                    channels_dir,
                    &dm_channel,
                    &format!("DM channel for {}", config.name),
                ) {
                    tracing::warn!(%e, channel = %dm_channel, "failed to create DM channel");
                }
            }
            events
        }
        Err(e) => {
            tracing::error!(%e, name = %config.name, "failed to spawn agent");
            vec![DomainEvent::AgentSpawnFailed {
                name: config.name.clone(),
                agent_type: config.agent_type.clone(),
                reason: e.to_string(),
            }]
        }
    }
}

/// Resume a stopped agent session. Returns AgentResumed events on success,
/// AgentSpawnFailed on failure.
async fn execute_resume(
    id: &str,
    projections: &Projections,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
) -> Vec<DomainEvent> {
    let agent = match projections.agents.by_id.get(id) {
        Some(a) => a,
        None => {
            tracing::warn!(%id, "resume requested for unknown agent");
            return vec![];
        }
    };
    let session_id = match &agent.session_id {
        Some(sid) => sid.clone(),
        None => {
            tracing::warn!(%id, "resume requested but agent has no session_id");
            return vec![];
        }
    };

    let config = spawn_config_from_agent(agent);
    let launch_config = spawn::build_launch_config(&config, paths.dir_key());
    let mut headless_config = launch_config.to_headless_config(paths);
    headless_config.resume_session_id = Some(session_id.clone());

    tracing::info!(%id, name = %agent.name, %session_id, "resuming agent session");

    match HeadlessSession::spawn(&headless_config).await {
        Ok(session) => {
            let pid = session.pid().unwrap_or(0);
            sessions.insert(id.to_string(), session);
            vec![DomainEvent::AgentResumed {
                id: id.to_string(),
                pid,
            }]
        }
        Err(e) => {
            tracing::error!(%id, %e, "failed to resume agent session");
            vec![DomainEvent::AgentSpawnFailed {
                name: agent.name.clone(),
                agent_type: agent.agent_type.clone(),
                reason: format!("resume failed: {e}"),
            }]
        }
    }
}

/// Execute a nudge: resolve action, ensure agent is alive, deliver message.
async fn execute_nudge(
    id: &str,
    message: &str,
    projections: &Projections,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
    channels_dir: &Path,
) -> Vec<DomainEvent> {
    let action = resolve_nudge_action(id, projections);
    eprintln!(
        "[DEBUG] execute_nudge id={id} action={action:?} sessions={:?}",
        sessions.keys().collect::<Vec<_>>()
    );
    let (events, target_id) = match action {
        NudgeAction::Deliver => (vec![], id.to_string()),
        NudgeAction::ResumeAndDeliver { .. } => {
            tracing::info!(%id, "resuming stopped agent for nudge");
            let events = execute_resume(id, projections, sessions, paths).await;
            (events, id.to_string())
        }
        NudgeAction::RespawnAndDeliver { config } => {
            tracing::info!(%id, name = %config.name, "respawning agent for nudge");
            let events = execute_spawn(&config, sessions, paths, channels_dir).await;
            // The new agent has a new ID from AgentCreated
            let new_id = events
                .iter()
                .find_map(|e| match e {
                    DomainEvent::AgentCreated { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| id.to_string());
            (events, new_id)
        }
        NudgeAction::Drop => {
            tracing::debug!(%id, "nudge target unknown, dropping");
            return vec![];
        }
    };

    // Deliver the message to whichever session is now alive
    deliver_nudge(sessions, &target_id, message).await;

    events
}

/// Send a message to an active session. Logs on failure but doesn't propagate.
async fn deliver_nudge(
    sessions: &mut HashMap<String, HeadlessSession>,
    agent_id: &str,
    message: &str,
) {
    if let Some(session) = sessions.get_mut(agent_id) {
        eprintln!("[DEBUG] deliver_nudge: sending to {agent_id}");
        match session.send_message(message).await {
            Ok(()) => eprintln!("[DEBUG] deliver_nudge: sent successfully to {agent_id}"),
            Err(e) => eprintln!("[DEBUG] deliver_nudge: FAILED for {agent_id}: {e}"),
        }
    } else {
        eprintln!("[DEBUG] deliver_nudge: NO SESSION for {agent_id} in sessions map");
    }
}

/// Detach stdout/stderr receivers and spawn a background drain task.
/// Without this, the child's stdout pipe fills up and the process blocks.
fn drain_session_output(session: &mut HeadlessSession, agent_name: &str) {
    if let Some((mut stdout_rx, mut stderr_rx)) = session.take_receivers() {
        let name = agent_name.to_string();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = stdout_rx.recv() => {
                        if msg.is_none() { break; }
                    }
                    msg = stderr_rx.recv() => {
                        match msg {
                            Some(line) if !line.trim().is_empty() => {
                                tracing::debug!(agent = %name, "stderr: {line}");
                            }
                            None => break,
                            _ => {}
                        }
                    }
                }
            }
            tracing::debug!(agent = %name, "stdout/stderr drain ended");
        });
    }
}
