use super::*;
use crate::daemon_v2::events::DomainEvent;
use crate::daemon_v2::projections::Projections;

fn make_projections_with_merged_pr(
    pr_number: u64,
    task_id: Option<&str>,
    task_status: Option<TaskStatus>,
) -> Projections {
    let mut proj = Projections::default();

    // Add the merged PR
    proj.apply(&DomainEvent::PrOpened {
        number: pr_number,
        branch: format!("feat/pr-{pr_number}"),
        author: "alice".into(),
    });
    proj.apply(&DomainEvent::PrMerged {
        number: pr_number,
        branch: format!("feat/pr-{pr_number}"),
    });

    // Optionally create and link a task
    if let Some(tid) = task_id {
        proj.apply(&DomainEvent::TaskCreated {
            id: tid.to_string(),
            subject: "test task".into(),
            channel: "general".into(),
            blocked_by: vec![],
        });

        if task_status == Some(TaskStatus::InProgress) {
            proj.apply(&DomainEvent::TaskAssigned {
                task_id: tid.to_string(),
                agent_id: "agent-1".into(),
            });
        }
        if task_status == Some(TaskStatus::Completed) {
            proj.apply(&DomainEvent::TaskAssigned {
                task_id: tid.to_string(),
                agent_id: "agent-1".into(),
            });
            proj.apply(&DomainEvent::TaskCompleted {
                task_id: tid.to_string(),
            });
        }

        proj.apply(&DomainEvent::PrLinkedToTask {
            number: pr_number,
            task_id: tid.to_string(),
        });
    }

    proj
}

#[test]
fn merged_pr_with_linked_in_progress_task_returns_complete() {
    let proj = make_projections_with_merged_pr(42, Some("task-1"), Some(TaskStatus::InProgress));

    let commands = handle_merged_prs(&proj);
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        &commands[0],
        Command::CompleteTask { task_id } if task_id == "task-1"
    ));
}

#[test]
fn merged_pr_without_task_returns_empty() {
    let proj = make_projections_with_merged_pr(42, None, None);

    let commands = handle_merged_prs(&proj);
    assert!(commands.is_empty());
}

#[test]
fn merged_pr_with_already_completed_task_returns_empty() {
    let proj = make_projections_with_merged_pr(42, Some("task-1"), Some(TaskStatus::Completed));

    let commands = handle_merged_prs(&proj);
    assert!(commands.is_empty());
}
