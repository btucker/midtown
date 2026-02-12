//! Task-based worktree registry.
//!
//! Maps tasks to worktrees by branch slug, decoupling worktree lifecycle from
//! coworker identity. This enables build cache reuse when tasks are reassigned
//! and automatic cleanup on PR merge.
//!
//! ## Path layout
//!
//! ```text
//! ~/.midtown/worktrees/<repo>/
//! └── task-42-add-auth-endpoint/   # branch slug = worktree_id
//! ```
//!
//! The registry is persisted as part of `DaemonPersistentState` in
//! `daemon-state.json` and can be reconstructed from a disk scan of the
//! worktrees directory.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single worktree assignment — connects a task to a worktree directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeAssignment {
    /// The worktree identifier (branch slug), used as the directory name.
    /// e.g., "task-42-add-auth-endpoint"
    pub worktree_id: String,
    /// The git branch name checked out in this worktree.
    /// Usually matches the worktree_id but may differ for review branches.
    pub branch_name: String,
    /// The task ID that owns this worktree (if any).
    pub task_id: Option<String>,
    /// The coworker currently bound to this worktree (if any).
    pub current_coworker: Option<String>,
    /// The PR number associated with this worktree (set when PR is opened).
    pub pr_number: Option<u64>,
    /// When the assignment was created.
    pub created_at: DateTime<Utc>,
    /// When the associated task was completed (for time-based cleanup).
    /// Set by the daemon when a task transitions to completed status.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
}

/// Registry tracking worktree assignments.
///
/// Maintains a primary map from worktree_id to assignment, plus reverse
/// indexes for fast lookups by task, coworker, and PR number.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorktreeRegistry {
    /// Primary map: worktree_id → assignment.
    assignments: HashMap<String, WorktreeAssignment>,
    /// Reverse index: task_id → worktree_id.
    #[serde(default)]
    task_index: HashMap<String, String>,
    /// Reverse index: coworker_name (lowercase) → worktree_id.
    #[serde(default)]
    coworker_index: HashMap<String, String>,
    /// Reverse index: pr_number → worktree_id.
    #[serde(default)]
    pr_index: HashMap<u64, String>,
}

impl WorktreeRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new worktree assignment.
    ///
    /// Returns an error if a worktree with the same ID already exists.
    pub fn assign_worktree(&mut self, assignment: WorktreeAssignment) -> Result<(), String> {
        let wt_id = assignment.worktree_id.clone();
        if self.assignments.contains_key(&wt_id) {
            return Err(format!("Worktree {} already registered", wt_id));
        }

        // Update reverse indexes
        if let Some(ref task_id) = assignment.task_id {
            self.task_index.insert(task_id.clone(), wt_id.clone());
        }
        if let Some(ref coworker) = assignment.current_coworker {
            self.coworker_index
                .insert(coworker.to_lowercase(), wt_id.clone());
        }
        if let Some(pr) = assignment.pr_number {
            self.pr_index.insert(pr, wt_id.clone());
        }

        self.assignments.insert(wt_id, assignment);
        Ok(())
    }

    /// Bind a coworker to an existing worktree.
    ///
    /// Enforces single-coworker-per-worktree: the old binding (if any) is
    /// removed before the new one is set.
    pub fn bind_coworker(&mut self, worktree_id: &str, coworker: &str) -> Result<(), String> {
        let assignment = self
            .assignments
            .get_mut(worktree_id)
            .ok_or_else(|| format!("Worktree {} not found in registry", worktree_id))?;

        // Remove old coworker binding if any
        if let Some(ref old) = assignment.current_coworker {
            self.coworker_index.remove(&old.to_lowercase());
        }

        assignment.current_coworker = Some(coworker.to_string());
        self.coworker_index
            .insert(coworker.to_lowercase(), worktree_id.to_string());
        Ok(())
    }

    /// Unbind a coworker from their worktree (worktree stays for reuse).
    pub fn unbind_coworker(&mut self, coworker: &str) {
        let key = coworker.to_lowercase();
        if let Some(wt_id) = self.coworker_index.remove(&key)
            && let Some(assignment) = self.assignments.get_mut(&wt_id)
            && assignment
                .current_coworker
                .as_ref()
                .is_some_and(|c| c.to_lowercase() == key)
        {
            assignment.current_coworker = None;
        }
    }

    /// Set the PR number for a worktree.
    pub fn set_pr_number(&mut self, worktree_id: &str, pr_number: u64) {
        if let Some(assignment) = self.assignments.get_mut(worktree_id) {
            // Remove old PR index if any
            if let Some(old_pr) = assignment.pr_number {
                self.pr_index.remove(&old_pr);
            }
            assignment.pr_number = Some(pr_number);
            self.pr_index.insert(pr_number, worktree_id.to_string());
        }
    }

    /// Mark a worktree's associated task as completed.
    ///
    /// Sets the `completed_at` timestamp to enable time-based cleanup.
    /// Called by the daemon when a task transitions to completed status.
    pub fn mark_task_completed(&mut self, task_id: &str, completed_at: DateTime<Utc>) {
        if let Some(wt_id) = self.task_index.get(task_id)
            && let Some(assignment) = self.assignments.get_mut(wt_id)
        {
            assignment.completed_at = Some(completed_at);
        }
    }

    /// Remove a worktree from the registry (e.g., after PR merge cleanup).
    pub fn remove_worktree(&mut self, worktree_id: &str) -> Option<WorktreeAssignment> {
        if let Some(assignment) = self.assignments.remove(worktree_id) {
            // Clean up reverse indexes
            if let Some(ref task_id) = assignment.task_id {
                self.task_index.remove(task_id);
            }
            if let Some(ref coworker) = assignment.current_coworker {
                self.coworker_index.remove(&coworker.to_lowercase());
            }
            if let Some(pr) = assignment.pr_number {
                self.pr_index.remove(&pr);
            }
            Some(assignment)
        } else {
            None
        }
    }

    /// Remove the worktree associated with a merged PR.
    ///
    /// Returns the assignment if found and removed.
    pub fn cleanup_for_merged_pr(&mut self, pr_number: u64) -> Option<WorktreeAssignment> {
        if let Some(wt_id) = self.pr_index.get(&pr_number).cloned() {
            self.remove_worktree(&wt_id)
        } else {
            None
        }
    }

    /// Look up a worktree by task ID.
    pub fn get_by_task(&self, task_id: &str) -> Option<&WorktreeAssignment> {
        self.task_index
            .get(task_id)
            .and_then(|wt_id| self.assignments.get(wt_id))
    }

    /// Look up a worktree by coworker name.
    pub fn get_by_coworker(&self, coworker: &str) -> Option<&WorktreeAssignment> {
        self.coworker_index
            .get(&coworker.to_lowercase())
            .and_then(|wt_id| self.assignments.get(wt_id))
    }

    /// Look up a worktree by PR number.
    pub fn get_by_pr(&self, pr_number: u64) -> Option<&WorktreeAssignment> {
        self.pr_index
            .get(&pr_number)
            .and_then(|wt_id| self.assignments.get(wt_id))
    }

    /// Look up a worktree by branch name.
    ///
    /// This is more precise than `get_by_coworker` when a coworker has multiple
    /// worktrees (one per task). Used when linking PRs to worktrees by matching
    /// the PR's headRefName to the worktree's branch_name.
    pub fn get_by_branch(&self, branch: &str) -> Option<&WorktreeAssignment> {
        self.assignments.values().find(|a| a.branch_name == branch)
    }

    /// Look up a worktree by its ID.
    pub fn get(&self, worktree_id: &str) -> Option<&WorktreeAssignment> {
        self.assignments.get(worktree_id)
    }

    /// Get all assignments (for iteration/debugging).
    pub fn all_assignments(&self) -> &HashMap<String, WorktreeAssignment> {
        &self.assignments
    }

    /// Number of registered worktrees.
    pub fn len(&self) -> usize {
        self.assignments.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.assignments.is_empty()
    }

    /// Rebuild reverse indexes from the primary assignments map.
    ///
    /// Call this after deserialization to ensure indexes are consistent,
    /// or after reconstructing from a disk scan.
    pub fn rebuild_indexes(&mut self) {
        self.task_index.clear();
        self.coworker_index.clear();
        self.pr_index.clear();

        for (wt_id, assignment) in &self.assignments {
            if let Some(ref task_id) = assignment.task_id {
                self.task_index.insert(task_id.clone(), wt_id.clone());
            }
            if let Some(ref coworker) = assignment.current_coworker {
                self.coworker_index
                    .insert(coworker.to_lowercase(), wt_id.clone());
            }
            if let Some(pr) = assignment.pr_number {
                self.pr_index.insert(pr, wt_id.clone());
            }
        }
    }
}

/// Generate a branch slug for a task.
///
/// Format: `task-<id>-<sanitized-subject>`
///
/// The subject is lowercased, non-alphanumeric characters replaced with hyphens,
/// consecutive hyphens collapsed, and the result truncated to ~50 characters
/// (at a word boundary).
pub fn branch_slug_for_task(task_id: &str, subject: &str) -> String {
    let prefix = format!("task-{}-", task_id);

    // Sanitize subject: lowercase, replace non-alnum with hyphens
    let sanitized: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens and trim leading/trailing hyphens
    let mut slug = String::new();
    let mut last_was_hyphen = false;
    for c in sanitized.chars() {
        if c == '-' {
            if !last_was_hyphen && !slug.is_empty() {
                slug.push('-');
            }
            last_was_hyphen = true;
        } else {
            slug.push(c);
            last_was_hyphen = false;
        }
    }
    let slug = slug.trim_end_matches('-');

    // Truncate to ~50 chars total (including prefix), at a hyphen boundary
    let max_slug_len = 50usize.saturating_sub(prefix.len());
    let truncated = if slug.len() > max_slug_len {
        // Find last hyphen before the limit
        match slug[..max_slug_len].rfind('-') {
            Some(pos) => &slug[..pos],
            None => &slug[..max_slug_len],
        }
    } else {
        slug
    };

    format!("{}{}", prefix, truncated)
}

/// Generate a branch slug for a PR review.
///
/// Format: `review-pr-<number>`
pub fn review_slug_for_pr(pr_number: u64) -> String {
    format!("review-pr-{}", pr_number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_slug_for_task_basic() {
        let slug = branch_slug_for_task("42", "Add auth endpoint");
        assert_eq!(slug, "task-42-add-auth-endpoint");
    }

    #[test]
    fn test_branch_slug_for_task_special_chars() {
        let slug = branch_slug_for_task("7", "Fix bug: handle `null` in API response!");
        assert_eq!(slug, "task-7-fix-bug-handle-null-in-api-response");
    }

    #[test]
    fn test_branch_slug_for_task_truncation() {
        let slug = branch_slug_for_task(
            "123",
            "This is a very long task subject that should be truncated at a reasonable boundary",
        );
        assert!(slug.len() <= 50, "slug too long: {} ({})", slug, slug.len());
        assert!(slug.starts_with("task-123-"));
        // Should truncate at a hyphen boundary
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn test_branch_slug_for_task_consecutive_special_chars() {
        let slug = branch_slug_for_task("1", "fix---multiple   spaces___and...dots");
        assert_eq!(slug, "task-1-fix-multiple-spaces-and-dots");
    }

    #[test]
    fn test_review_slug_for_pr() {
        assert_eq!(review_slug_for_pr(42), "review-pr-42");
    }

    #[test]
    fn test_registry_assign_and_lookup() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("lexington".to_string()),
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();

        // Lookup by worktree_id
        assert!(registry.get("task-42-add-auth").is_some());

        // Lookup by task
        let by_task = registry.get_by_task("42").unwrap();
        assert_eq!(by_task.worktree_id, "task-42-add-auth");

        // Lookup by coworker
        let by_coworker = registry.get_by_coworker("lexington").unwrap();
        assert_eq!(by_coworker.worktree_id, "task-42-add-auth");
    }

    #[test]
    fn test_registry_duplicate_assignment_fails() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment.clone()).unwrap();
        assert!(registry.assign_worktree(assignment).is_err());
    }

    #[test]
    fn test_registry_bind_unbind_coworker() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();

        // Bind coworker
        registry.bind_coworker("task-42-add-auth", "park").unwrap();
        assert_eq!(
            registry.get_by_coworker("park").unwrap().worktree_id,
            "task-42-add-auth"
        );

        // Unbind coworker
        registry.unbind_coworker("park");
        assert!(registry.get_by_coworker("park").is_none());
        // Worktree still exists
        assert!(registry.get("task-42-add-auth").is_some());
    }

    #[test]
    fn test_registry_bind_replaces_old_coworker() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("lexington".to_string()),
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();

        // Bind a different coworker — should replace
        registry.bind_coworker("task-42-add-auth", "park").unwrap();
        assert!(registry.get_by_coworker("lexington").is_none());
        assert!(registry.get_by_coworker("park").is_some());
    }

    #[test]
    fn test_registry_set_pr_number() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();
        registry.set_pr_number("task-42-add-auth", 123);

        let by_pr = registry.get_by_pr(123).unwrap();
        assert_eq!(by_pr.worktree_id, "task-42-add-auth");
    }

    #[test]
    fn test_registry_cleanup_for_merged_pr() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("lexington".to_string()),
            pr_number: Some(123),
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();
        assert_eq!(registry.len(), 1);

        let removed = registry.cleanup_for_merged_pr(123).unwrap();
        assert_eq!(removed.worktree_id, "task-42-add-auth");
        assert_eq!(registry.len(), 0);

        // All indexes should be cleaned up
        assert!(registry.get_by_task("42").is_none());
        assert!(registry.get_by_coworker("lexington").is_none());
        assert!(registry.get_by_pr(123).is_none());
    }

    #[test]
    fn test_registry_remove_worktree() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("lexington".to_string()),
            pr_number: Some(123),
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();
        let removed = registry.remove_worktree("task-42-add-auth").unwrap();
        assert_eq!(removed.task_id, Some("42".to_string()));
        assert!(registry.is_empty());
    }

    #[test]
    fn test_registry_rebuild_indexes() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("lexington".to_string()),
            pr_number: Some(123),
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();

        // Clear indexes to simulate deserialization without indexes
        registry.task_index.clear();
        registry.coworker_index.clear();
        registry.pr_index.clear();

        // Rebuild should restore them
        registry.rebuild_indexes();

        assert!(registry.get_by_task("42").is_some());
        assert!(registry.get_by_coworker("lexington").is_some());
        assert!(registry.get_by_pr(123).is_some());
    }

    #[test]
    fn test_registry_serde_roundtrip() {
        let mut registry = WorktreeRegistry::new();

        let assignment = WorktreeAssignment {
            worktree_id: "task-42-add-auth".to_string(),
            branch_name: "task-42-add-auth".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: Some("lexington".to_string()),
            pr_number: Some(123),
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();

        let json = serde_json::to_string_pretty(&registry).unwrap();
        let mut loaded: WorktreeRegistry = serde_json::from_str(&json).unwrap();
        loaded.rebuild_indexes();

        assert_eq!(loaded.len(), 1);
        assert!(loaded.get_by_task("42").is_some());
        assert!(loaded.get_by_coworker("lexington").is_some());
        assert!(loaded.get_by_pr(123).is_some());
    }

    #[test]
    fn test_registry_get_by_branch_finds_correct_worktree() {
        let mut registry = WorktreeRegistry::new();

        // Simulate a coworker with multiple task worktrees
        let task_948 = WorktreeAssignment {
            worktree_id: "task-948-old-work".to_string(),
            branch_name: "task-948-old-work".to_string(),
            task_id: Some("948".to_string()),
            current_coworker: Some("amsterdam".to_string()),
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        let task_1011 = WorktreeAssignment {
            worktree_id: "task-1011-fix-bug".to_string(),
            branch_name: "task-1011-fix-bug".to_string(),
            task_id: Some("1011".to_string()),
            current_coworker: Some("amsterdam".to_string()),
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(task_948).unwrap();
        registry.assign_worktree(task_1011).unwrap();

        // get_by_coworker can only return ONE worktree (whichever was bound last)
        // This is the bug: it's ambiguous which worktree belongs to the coworker
        let by_coworker = registry.get_by_coworker("amsterdam");
        assert!(by_coworker.is_some());

        // But get_by_branch should find the exact worktree we want
        let by_branch = registry.get_by_branch("task-1011-fix-bug");
        assert!(by_branch.is_some());
        assert_eq!(by_branch.unwrap().worktree_id, "task-1011-fix-bug");

        // Simulate opening a PR on task-1011's branch
        // The fix: use branch-based lookup instead of get_by_coworker
        registry.set_pr_number("task-1011-fix-bug", 813);

        let by_pr = registry.get_by_pr(813).unwrap();
        assert_eq!(by_pr.worktree_id, "task-1011-fix-bug");
        assert_eq!(by_pr.pr_number, Some(813));
    }

    #[test]
    fn test_mark_task_completed() {
        let mut registry = WorktreeRegistry::new();

        // Create a worktree assignment
        let assignment = WorktreeAssignment {
            worktree_id: "task-42-feature".to_string(),
            branch_name: "task-42-feature".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: Utc::now(),
            completed_at: None,
        };

        registry.assign_worktree(assignment).unwrap();

        // Verify it starts with no completion time
        let assignment = registry.get_by_task("42").unwrap();
        assert!(assignment.completed_at.is_none());

        // Mark it as completed
        let completed_at = Utc::now();
        registry.mark_task_completed("42", completed_at);

        // Verify the completion time was set
        let assignment = registry.get_by_task("42").unwrap();
        assert!(assignment.completed_at.is_some());
        assert_eq!(assignment.completed_at.unwrap(), completed_at);
    }

    #[test]
    fn test_mark_task_completed_nonexistent_task() {
        let mut registry = WorktreeRegistry::new();

        // Marking a non-existent task as completed should be a no-op (not panic)
        registry.mark_task_completed("999", Utc::now());

        assert!(registry.get_by_task("999").is_none());
    }
}
