//! End-to-end tests for task sharing between Lead and coworkers.
//!
//! Tests the symlink-based approach where coworkers' task directories
//! are symlinked to the Lead's task directory, enabling shared task state.
//!
//! These tests simulate the directory structure without modifying HOME,
//! testing the core symlink logic that enables task sharing.

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Test helper: Create a mock directory structure for testing task sharing.
struct TaskSharingFixture {
    temp: TempDir,
    midtown_dir: PathBuf,
    tasks_dir: PathBuf,
}

impl TaskSharingFixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("Failed to create temp dir");
        let base = temp.path().to_path_buf();

        let midtown_dir = base.join(".midtown");
        let tasks_dir = base.join(".claude").join("tasks");

        fs::create_dir_all(&midtown_dir).unwrap();
        fs::create_dir_all(&tasks_dir).unwrap();

        Self {
            temp,
            midtown_dir,
            tasks_dir,
        }
    }

    /// Create a Lead session ID file for a repo
    fn create_lead_session(&self, repo_name: &str, session_id: &str) -> PathBuf {
        let repo_dir = self.midtown_dir.join(repo_name);
        fs::create_dir_all(&repo_dir).unwrap();
        let session_file = repo_dir.join("lead-session-id");
        fs::write(&session_file, session_id).unwrap();
        session_file
    }

    /// Get path to a session's task directory
    fn session_tasks_dir(&self, session_id: &str) -> PathBuf {
        self.tasks_dir.join(session_id)
    }

    /// Create a task file in a session's task directory
    fn create_task(&self, session_id: &str, task_id: &str, content: &str) {
        let task_dir = self.session_tasks_dir(session_id);
        fs::create_dir_all(&task_dir).unwrap();
        fs::write(task_dir.join(format!("{}.json", task_id)), content).unwrap();
    }

    /// Create a symlink from coworker's task dir to lead's task dir
    /// This simulates what midtown does when spawning a coworker
    fn symlink_coworker_to_lead(&self, coworker_session_id: &str, lead_session_id: &str) {
        let lead_tasks = self.session_tasks_dir(lead_session_id);
        let coworker_tasks = self.session_tasks_dir(coworker_session_id);

        // Ensure lead's directory exists
        fs::create_dir_all(&lead_tasks).unwrap();

        // Remove existing symlink if present
        if coworker_tasks.exists() || coworker_tasks.is_symlink() {
            if coworker_tasks.is_symlink() {
                fs::remove_file(&coworker_tasks).unwrap();
            }
        }

        // Create symlink: coworker -> lead
        #[cfg(unix)]
        std::os::unix::fs::symlink(&lead_tasks, &coworker_tasks).unwrap();
    }

    /// Read a task from a session's task directory
    fn read_task(&self, session_id: &str, task_id: &str) -> Option<String> {
        let task_path = self
            .session_tasks_dir(session_id)
            .join(format!("{}.json", task_id));
        fs::read_to_string(task_path).ok()
    }

    /// Check if a session's task path is a symlink
    fn is_symlink(&self, session_id: &str) -> bool {
        self.session_tasks_dir(session_id).is_symlink()
    }

    /// Get the base temp directory path (for debugging)
    #[allow(dead_code)]
    fn base_path(&self) -> &std::path::Path {
        self.temp.path()
    }
}

#[test]
fn test_task_sharing_e2e_coworker_sees_lead_tasks() {
    let fixture = TaskSharingFixture::new();
    let repo_name = "test-project";
    let lead_session = "lead-abc-123";
    let coworker_session = "coworker-def-456";

    // Step 1: Lead starts and creates session
    fixture.create_lead_session(repo_name, lead_session);

    // Step 2: Lead creates some tasks
    fixture.create_task(
        lead_session,
        "1",
        r#"{"id": "1", "subject": "Implement auth", "status": "pending"}"#,
    );
    fixture.create_task(
        lead_session,
        "2",
        r#"{"id": "2", "subject": "Add tests", "status": "pending"}"#,
    );

    // Step 3: Coworker spawns, symlink is created
    fixture.symlink_coworker_to_lead(coworker_session, lead_session);

    // Verify: Coworker can see Lead's tasks
    let task1 = fixture.read_task(coworker_session, "1");
    assert!(task1.is_some(), "Coworker should see Lead's task 1");
    assert!(task1.unwrap().contains("Implement auth"));

    let task2 = fixture.read_task(coworker_session, "2");
    assert!(task2.is_some(), "Coworker should see Lead's task 2");
    assert!(task2.unwrap().contains("Add tests"));

    // Verify: Coworker's task dir is a symlink
    assert!(
        fixture.is_symlink(coworker_session),
        "Coworker task dir should be a symlink"
    );
}

#[test]
fn test_task_sharing_e2e_coworker_writes_visible_to_lead() {
    let fixture = TaskSharingFixture::new();
    let repo_name = "test-project-2";
    let lead_session = "lead-xyz-789";
    let coworker_session = "coworker-uvw-012";

    // Setup: Lead session and symlink
    fixture.create_lead_session(repo_name, lead_session);
    fixture.symlink_coworker_to_lead(coworker_session, lead_session);

    // Coworker creates a task (simulating TaskCreate)
    fixture.create_task(
        coworker_session,
        "3",
        r#"{"id": "3", "subject": "Coworker task", "status": "in_progress"}"#,
    );

    // Verify: Lead can see the task created by coworker
    let task3 = fixture.read_task(lead_session, "3");
    assert!(task3.is_some(), "Lead should see task created by coworker");
    assert!(task3.unwrap().contains("Coworker task"));
}

#[test]
fn test_task_sharing_e2e_multiple_coworkers_share_tasks() {
    let fixture = TaskSharingFixture::new();
    let repo_name = "test-project-3";
    let lead_session = "lead-multi-001";
    let coworker1_session = "lexington-002";
    let coworker2_session = "park-003";

    // Setup: Lead session and symlinks for two coworkers
    fixture.create_lead_session(repo_name, lead_session);
    fixture.symlink_coworker_to_lead(coworker1_session, lead_session);
    fixture.symlink_coworker_to_lead(coworker2_session, lead_session);

    // Lead creates initial tasks
    fixture.create_task(
        lead_session,
        "1",
        r#"{"id": "1", "subject": "Task A", "status": "pending"}"#,
    );

    // Coworker 1 claims task and creates a new one
    fixture.create_task(
        coworker1_session,
        "1",
        r#"{"id": "1", "subject": "Task A", "status": "in_progress", "owner": "lexington"}"#,
    );
    fixture.create_task(
        coworker1_session,
        "2",
        r#"{"id": "2", "subject": "Found bug", "status": "pending"}"#,
    );

    // Verify: Coworker 2 sees the updated task 1 and new task 2
    let task1 = fixture.read_task(coworker2_session, "1").unwrap();
    assert!(
        task1.contains("in_progress"),
        "Coworker 2 should see task 1 is now in_progress"
    );
    assert!(
        task1.contains("lexington"),
        "Coworker 2 should see lexington owns task 1"
    );

    let task2 = fixture.read_task(coworker2_session, "2").unwrap();
    assert!(
        task2.contains("Found bug"),
        "Coworker 2 should see task 2 created by coworker 1"
    );

    // Verify: Lead also sees all updates
    let lead_task1 = fixture.read_task(lead_session, "1").unwrap();
    assert!(lead_task1.contains("lexington"));

    let lead_task2 = fixture.read_task(lead_session, "2").unwrap();
    assert!(lead_task2.contains("Found bug"));
}

#[test]
fn test_task_sharing_e2e_session_id_persistence() {
    let fixture = TaskSharingFixture::new();
    let repo_name = "persistent-repo";
    let session_id = "persistent-session-123";

    // Create session ID
    let session_file = fixture.create_lead_session(repo_name, session_id);

    // Read it back (simulating what get_lead_session_id does)
    let read_session = fs::read_to_string(&session_file)
        .unwrap()
        .trim()
        .to_string();

    assert_eq!(read_session, session_id);

    // Verify it survives "restart" (re-reading)
    let read_again = fs::read_to_string(&session_file)
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(read_again, session_id);
}

#[test]
fn test_task_sharing_e2e_symlink_recreation_on_restart() {
    let fixture = TaskSharingFixture::new();
    let lead_session = "lead-restart-001";
    let coworker_session = "coworker-restart-002";

    // Create lead tasks dir with a task
    fixture.create_task(
        lead_session,
        "1",
        r#"{"id": "1", "subject": "Original task"}"#,
    );

    // Create symlink (first spawn)
    fixture.symlink_coworker_to_lead(coworker_session, lead_session);

    // Verify symlink works
    assert!(fixture.read_task(coworker_session, "1").is_some());

    // Simulate coworker restart - recreate symlink
    // (symlink_coworker_to_lead handles removing existing symlink)
    fixture.symlink_coworker_to_lead(coworker_session, lead_session);

    // Should still work after "restart"
    let task = fixture.read_task(coworker_session, "1");
    assert!(task.is_some());
    assert!(task.unwrap().contains("Original task"));
}

#[test]
fn test_task_sharing_e2e_both_symlinks_point_to_same_data() {
    let fixture = TaskSharingFixture::new();
    let lead_session = "lead-same-001";
    let coworker1_session = "lexington-same-002";
    let coworker2_session = "park-same-003";

    // Setup symlinks
    fixture.symlink_coworker_to_lead(coworker1_session, lead_session);
    fixture.symlink_coworker_to_lead(coworker2_session, lead_session);

    // Coworker 1 writes a task
    fixture.create_task(
        coworker1_session,
        "shared",
        r#"{"id": "shared", "from": "lexington"}"#,
    );

    // Coworker 2 should see it immediately (same underlying directory)
    let from_coworker2 = fixture.read_task(coworker2_session, "shared");
    assert!(from_coworker2.is_some());
    assert!(from_coworker2.unwrap().contains("lexington"));

    // Lead should see it too
    let from_lead = fixture.read_task(lead_session, "shared");
    assert!(from_lead.is_some());
    assert!(from_lead.unwrap().contains("lexington"));

    // Modify via lead
    fixture.create_task(
        lead_session,
        "shared",
        r#"{"id": "shared", "from": "lexington", "reviewed_by": "lead"}"#,
    );

    // Both coworkers see the update
    let updated1 = fixture.read_task(coworker1_session, "shared").unwrap();
    let updated2 = fixture.read_task(coworker2_session, "shared").unwrap();

    assert!(updated1.contains("reviewed_by"));
    assert!(updated2.contains("reviewed_by"));
}
