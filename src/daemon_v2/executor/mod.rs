pub mod channel_io;
pub mod github;
pub mod spawn;

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
        other => {
            tracing::debug!(?other, "unhandled command");
            vec![]
        }
    }
}
