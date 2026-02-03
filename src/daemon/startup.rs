//! Startup state recovery for the midtown daemon.
//!
//! Handles recovery of coworker workflow state across daemon restarts.
//! When the daemon starts, it discovers running coworkers from tmux and
//! recovers their workflow phases from individual state files.

use std::collections::HashMap;

use tokio::sync::RwLock;
use tracing::info;

use crate::coworker::CoworkerManager;
use crate::rules::CoworkerRecord;

/// Recover coworker workflow state from their state files.
///
/// For each coworker discovered in the tmux session, reads their state.json
/// to recover:
/// - Workflow phase (developing, testing, pull-request, etc.)
/// - Current task ID
/// - Last workflow update timestamp
///
/// This allows the daemon to resume coordination of in-progress work
/// after a restart without losing context about what each coworker was doing.
pub async fn recover_coworker_records(
    repo_name: &str,
    coworkers: &CoworkerManager,
    coworker_records: &RwLock<HashMap<String, CoworkerRecord>>,
) {
    let discovered_names: Vec<String> = coworkers.list().iter().map(|c| c.name.clone()).collect();

    if discovered_names.is_empty() {
        return;
    }

    let mut records = coworker_records.write().await;
    for name in &discovered_names {
        if let Some(file_state) = crate::coworker_state::read_state(repo_name, name) {
            info!(
                "Recovered state for {}: {} (from state.json)",
                name,
                file_state.display_status()
            );
            let mut record = CoworkerRecord::new_spawn();
            record.workflow_phase = Some(file_state.phase);
            record.task_id = file_state.task_id;
            record.workflow_updated_at = Some(file_state.updated_at);
            records.insert(name.to_string(), record);
        } else {
            // No state file - create a minimal record so the coworker is tracked
            records.insert(name.to_string(), CoworkerRecord::new_spawn());
        }
    }
}

// Unit tests for this module would require mocking CoworkerManager, which is complex.
// The behavior is covered by E2E tests that verify daemon restart recovery works correctly.
// See tests/daemon_e2e.rs for integration coverage.
