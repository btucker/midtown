pub mod spawn;

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::DomainEvent;

pub async fn execute(command: Command, _dir_key: &str) -> Vec<DomainEvent> {
    match command {
        Command::StopAgent { id, reason } => {
            tracing::info!(%id, %reason, "would stop agent");
            vec![DomainEvent::AgentStopped { id, reason }]
        }
        Command::ResetTask { task_id } => {
            tracing::info!(%task_id, "resetting task to pending");
            vec![DomainEvent::TaskReset {
                task_id,
                reason: "agent died".into(),
            }]
        }
        _ => {
            tracing::debug!(?command, "unhandled command");
            vec![]
        }
    }
}
