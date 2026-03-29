pub mod channel_io;
pub mod github;
pub mod spawn;
pub mod webhook;

use std::collections::HashMap;
use std::path::Path;

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::DomainEvent;
use crate::daemon_v2::projections::Projections;
use crate::headless::HeadlessSession;
use crate::paths::ProjectPaths;

pub async fn execute(
    command: Command,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
    projections: &Projections,
    channels_dir: &Path,
) -> Vec<DomainEvent> {
    match command {
        Command::SpawnAgent(config) => match spawn::spawn_agent(&config, paths).await {
            Ok((session, events)) => {
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
            if let Some(session) = sessions.get_mut(&id) {
                if let Err(e) = session.send_message(&message).await {
                    tracing::error!(%id, %e, "failed to nudge agent");
                }
            } else {
                tracing::warn!(%id, "nudge target not found in sessions");
            }
            vec![] // nudges don't produce events
        }
        Command::ResumeAgent { id } => {
            let agent = match projections.agents.by_id.get(&id) {
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

            // Build a SpawnConfig so we can reuse build_launch_config
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
                "resuming agent session after daemon restart"
            );

            match HeadlessSession::spawn(&headless_config).await {
                Ok(session) => {
                    let pid = session.pid().unwrap_or(0);
                    sessions.insert(id.clone(), session);
                    vec![DomainEvent::AgentResumed { id, pid }]
                }
                Err(e) => {
                    tracing::error!(%id, %e, "failed to resume agent session");
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
    }
}
