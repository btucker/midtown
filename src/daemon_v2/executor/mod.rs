pub mod channel_io;
pub mod github;
pub mod spawn;
pub mod webhook;

#[path = "nudge_tests.rs"]
#[cfg(test)]
mod nudge_tests;

use std::collections::HashMap;
use std::path::Path;

use crate::daemon_v2::decisions::Command;
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
    /// Agent can't be nudged (unknown, no session_id, etc).
    Drop,
}

/// Determine the nudge strategy for a given agent.
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
        NudgeAction::Drop
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
        Command::SpawnAgent(config) => match spawn::spawn_agent(&config, paths).await {
            Ok((mut session, events)) => {
                // Detach stdout/stderr receivers and spawn a background drain task.
                // Without this, the child's stdout pipe fills up and the process blocks.
                if let Some((mut stdout_rx, mut stderr_rx)) = session.take_receivers() {
                    let agent_name = config.name.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                msg = stdout_rx.recv() => {
                                    if msg.is_none() { break; }
                                    // Drained — output is persisted by Claude Code itself
                                }
                                msg = stderr_rx.recv() => {
                                    match msg {
                                        Some(line) if !line.trim().is_empty() => {
                                            tracing::debug!(agent = %agent_name, "stderr: {line}");
                                        }
                                        None => break,
                                        _ => {}
                                    }
                                }
                            }
                        }
                        tracing::debug!(agent = %agent_name, "stdout/stderr drain ended");
                    });
                }

                // The agent ID is in the first event (AgentCreated).
                if let Some(DomainEvent::AgentCreated { id, .. }) = events.first() {
                    sessions.insert(id.clone(), session);
                }
                // Auto-create DM channel for workers so they can communicate privately.
                if config.kind == crate::daemon_v2::events::AgentKind::Worker {
                    let dm_channel = crate::daemon_v2::decisions::lifecycle::create_dm_channel_name(
                        &config.name,
                    );
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
                vec![]
            }
        },
        Command::StopAgent { id, reason } => {
            if let Some(mut session) = sessions.remove(&id)
                && let Err(e) = spawn::stop_agent(&mut session).await
            {
                tracing::warn!(%id, %e, "error stopping agent");
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
            match resolve_nudge_action(&id, projections) {
                NudgeAction::Deliver => {
                    if let Some(session) = sessions.get_mut(&id)
                        && let Err(e) = session.send_message(&message).await
                    {
                        tracing::error!(%id, %e, "failed to nudge agent");
                    }
                    vec![]
                }
                NudgeAction::ResumeAndDeliver { session_id } => {
                    tracing::info!(%id, %session_id, "resuming stopped agent for nudge");
                    match resume_agent(&id, projections, sessions, paths).await {
                        Ok(events) => {
                            // Deliver the nudge to the now-resumed session
                            if let Some(session) = sessions.get_mut(&id)
                                && let Err(e) = session.send_message(&message).await
                            {
                                tracing::error!(%id, %e, "failed to nudge resumed agent");
                            }
                            events
                        }
                        Err(e) => {
                            tracing::error!(%id, %e, "failed to resume agent for nudge");
                            vec![]
                        }
                    }
                }
                NudgeAction::Drop => {
                    tracing::debug!(%id, "nudge target cannot be resumed, dropping");
                    vec![]
                }
            }
        }
        Command::ResumeAgent { id } => {
            match resume_agent(&id, projections, sessions, paths).await {
                Ok(events) => events,
                Err(e) => {
                    tracing::error!(%id, %e, "failed to resume agent");
                    vec![]
                }
            }
        }
        Command::AssignTask { task_id, agent_id } => {
            vec![DomainEvent::TaskAssigned { task_id, agent_id }]
        }
        Command::CompleteTask { task_id } => {
            vec![DomainEvent::TaskCompleted { task_id }]
        }
        Command::CreateWorktree { task_id, branch } => {
            // Worktree creation is handled in DaemonV2::prepare_worktree_for_spawn()
            // before SpawnAgent commands are executed. This arm exists for explicit
            // worktree creation requests that bypass the spawn path.
            tracing::debug!(%task_id, %branch, "CreateWorktree command received (worktree lifecycle managed by daemon)");
            vec![]
        }
        Command::RemoveWorktree { task_id } => {
            // Worktree removal is handled in DaemonV2::handle_worktree_cleanup()
            // after TaskCompleted events. This arm exists for explicit removal
            // requests that bypass the task completion path.
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

/// Resume a stopped agent session. Returns AgentResumed events on success.
async fn resume_agent(
    id: &str,
    projections: &Projections,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
) -> Result<Vec<DomainEvent>, String> {
    let agent = projections
        .agents
        .by_id
        .get(id)
        .ok_or_else(|| format!("unknown agent {id}"))?;
    let session_id = agent
        .session_id
        .as_ref()
        .ok_or_else(|| format!("agent {id} has no session_id"))?
        .clone();

    let spawn_config = crate::daemon_v2::decisions::SpawnConfig {
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
    };

    let launch_config = spawn::build_launch_config(&spawn_config, paths.dir_key());
    let mut headless_config = launch_config.to_headless_config(paths);
    headless_config.resume_session_id = Some(session_id.clone());

    tracing::info!(
        %id, name = %agent.name, %session_id,
        "resuming agent session"
    );

    match HeadlessSession::spawn(&headless_config).await {
        Ok(session) => {
            let pid = session.pid().unwrap_or(0);
            sessions.insert(id.to_string(), session);
            Ok(vec![DomainEvent::AgentResumed {
                id: id.to_string(),
                pid,
            }])
        }
        Err(e) => Err(format!("spawn failed: {e}")),
    }
}
