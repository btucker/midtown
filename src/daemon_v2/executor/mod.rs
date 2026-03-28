pub mod spawn;

use std::collections::HashMap;

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::DomainEvent;
use crate::headless::HeadlessSession;
use crate::paths::ProjectPaths;

pub async fn execute(
    command: Command,
    sessions: &mut HashMap<String, HeadlessSession>,
    paths: &ProjectPaths,
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
        other => {
            tracing::debug!(?other, "unhandled command");
            vec![]
        }
    }
}
