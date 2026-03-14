//! Path utilities for midtown daemon and clients.
//!
//! Provides consistent path handling for daemon sockets, channels, and other
//! resources, with proper support for git worktrees.
//!
//! ## Directory Structure
//!
//! The `~/.midtown/` directory is organized as follows:
//!
//! ```text
//! ~/.midtown/
//! ├── config.toml                    # Global configuration (read-only in sandbox)
//! ├── agents/                        # Custom agent prompts (read-only in sandbox)
//! └── projects/                      # Project-specific runtime data
//!     └── <repo>/
//!         ├── lead-session-id        # Lead's Claude session ID
//!         ├── lead-system-prompt.txt # Lead's system prompt for attach resumption
//!         ├── worktrees/             # Task-based worktrees (named by branch slug)
//!         │   ├── lead/              # Lead worktree
//!         │   └── <slug>/            # e.g., task-42-add-auth-endpoint
//!         ├── coworkers/             # Legacy coworker worktrees (named by coworker)
//!         │   └── <name>/            # Individual worktree
//!         ├── workflow.py            # Project-level default workflow script (local, optional)
//!         ├── channels/              # Per-channel directories
//!         │   └── <name>/            # e.g., "midtown", "features"
//!         │       ├── history/current.jsonl  # Active message log
//!         │       ├── history/YYYY-MM-DD.jsonl # Rotated daily archives
//!         │       ├── notes/                 # Channel lead knowledge files
//!         │       ├── cursors/               # Per-agent read cursors
//!         │       ├── directory              # Channel working directory override (optional)
//!         │       ├── workflow.py            # Channel-specific workflow script (local, optional)
//!         │       └── workflow-state.json    # Legacy (migrated to daemon-state.json on startup)
//!         ├── workflows/             # Named workflow definitions
//!         │   └── <name>/            # e.g., "tdw", "spec-review"
//!         │       ├── workflow.py    # Workflow hooks (required)
//!         │       └── AGENTS.md     # Workflow docs for channel lead (optional)
//!         ├── sessions/              # Headless session transcripts (headless-<name>.jsonl)
//!         ├── logs/                  # Daemon logs
//!         ├── daemon.pid             # Daemon PID file
//!         └── assets/                # Coworker-generated screenshots and videos
//! ```
//!
//! ## Workflows
//!
//! Named workflows live in `~/.midtown/projects/<repo>/workflows/<name>/` with
//! `workflow.py` (hooks, required) and optional `AGENTS.md` (channel lead instructions).
//! Channels reference workflows by name via `channel_workflows` in daemon state.
//! See [`workflow_agents_md_content`] and [`merge_workflow_agents_md`].

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
thread_local! {
    static TEST_MIDTOWN_BASE_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
    static TEST_MIDTOWN_DATA_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_midtown_base_dir_override() -> Option<PathBuf> {
    TEST_MIDTOWN_BASE_DIR.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
fn test_midtown_data_dir_override() -> Option<PathBuf> {
    TEST_MIDTOWN_DATA_DIR.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
pub struct TestMidtownBaseDirGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl Drop for TestMidtownBaseDirGuard {
    fn drop(&mut self) {
        TEST_MIDTOWN_BASE_DIR.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
pub fn set_test_midtown_base_dir(path: PathBuf) -> TestMidtownBaseDirGuard {
    let previous = TEST_MIDTOWN_BASE_DIR.with(|slot| slot.replace(Some(path)));
    TestMidtownBaseDirGuard { previous }
}

#[cfg(test)]
pub struct TestMidtownDataDirGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
impl Drop for TestMidtownDataDirGuard {
    fn drop(&mut self) {
        TEST_MIDTOWN_DATA_DIR.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

#[cfg(test)]
pub fn set_test_midtown_data_dir(path: PathBuf) -> TestMidtownDataDirGuard {
    let previous = TEST_MIDTOWN_DATA_DIR.with(|slot| slot.replace(Some(path)));
    TestMidtownDataDirGuard { previous }
}

/// Detect the current git repository name.
///
/// If `MIDTOWN_DIR_KEY` is set (injected by the daemon when spawning leads
/// and coworkers), returns that value directly — pinning project identity
/// regardless of CWD. This prevents cross-project data leaks when a session
/// `cd`s into a different repo.
///
/// Otherwise falls back to CWD-based git detection using
/// `git rev-parse --git-common-dir` to handle worktrees correctly.
///
/// Returns `None` if not in a git repository (and no env override).
pub fn detect_repo_name() -> Option<String> {
    if let Ok(dir_key) = std::env::var("MIDTOWN_DIR_KEY")
        && !dir_key.is_empty()
    {
        // Validate: reject values containing path separators or traversal components
        // to prevent escaping ~/.midtown/projects/<dir_key>/ scope.
        if dir_key.contains('/')
            || dir_key.contains('\\')
            || dir_key == "."
            || dir_key == ".."
            || dir_key.contains("..")
        {
            tracing::warn!(
                dir_key,
                "MIDTOWN_DIR_KEY contains invalid characters, falling back to CWD detection"
            );
        } else {
            return Some(dir_key);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    detect_repo_name_from_dir(&cwd)
}

/// Detect the repository name from a specific directory (CWD-independent).
pub fn detect_repo_name_from_dir(dir: &std::path::Path) -> Option<String> {
    // First try git-common-dir which works correctly for worktrees
    let common_dir = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(dir)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        });

    if let Some(git_dir) = common_dir {
        let git_path = std::path::Path::new(&git_dir);
        // The git-common-dir is the .git folder - get its parent's name
        if let Some(parent) = git_path.parent() {
            // Handle relative ".git" by getting the actual toplevel
            if git_dir == ".git" {
                return std::process::Command::new("git")
                    .args(["rev-parse", "--show-toplevel"])
                    .current_dir(dir)
                    .output()
                    .ok()
                    .and_then(|output| {
                        if output.status.success() {
                            let path = String::from_utf8_lossy(&output.stdout);
                            std::path::Path::new(path.trim())
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                        } else {
                            None
                        }
                    });
            }
            // For worktrees, parent is the main repo directory
            return parent.file_name().map(|s| s.to_string_lossy().to_string());
        }
    }

    // Fallback: try show-toplevel for regular repos
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                std::path::Path::new(path.trim())
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
}

/// Detect the project name for the current repository.
///
/// Priority:
/// 1. `[project].name` from the per-project config.toml
/// 2. Auto-sanitized repo directory name (dots replaced with hyphens)
///
/// This is the canonical way to get the project name for use in
/// session names, channel names, display labels, etc.
pub fn detect_project_name() -> Option<String> {
    let repo_name = detect_repo_name()?;
    Some(project_name_for_dir_key(&repo_name))
}

/// Derive a project name from a dir_key (git directory name).
///
/// Resolution order:
/// 1. `[project].name` from config.toml (explicit override)
/// 2. `dir_key.replace('.', '-')` (auto-sanitized from git dir name)
///
/// This ensures project names are always valid identifiers (no dots)
/// without requiring manual config.
pub fn project_name_for_dir_key(dir_key: &str) -> String {
    if let Some(name) = crate::config::load_full_project_config(dir_key)
        .and_then(|c| c.project.name().map(|s| s.to_string()))
    {
        return name;
    }
    sanitize_project_name(dir_key)
}

/// Sanitize a directory name into a valid project name by replacing dots with hyphens.
pub fn sanitize_project_name(dir_key: &str) -> String {
    dir_key.replace('.', "-")
}

/// Consolidated path manager for a project.
///
/// Carries both identifiers that a project needs:
/// - `dir_key`: The git directory name (e.g., "midtown.nosync"), used for filesystem paths
/// - `project_name`: The logical identity (e.g., "midtown"), used for channel names, display, etc.
///
/// All path methods that previously existed as `*_for_repo()` free functions
/// are now methods on this struct, ensuring consistent path construction.
#[derive(Debug, Clone)]
pub struct ProjectPaths {
    dir_key: String,
    project_name: String,
    base: PathBuf,
    state_base: PathBuf,
}

impl ProjectPaths {
    /// Create `ProjectPaths` from a dir_key, resolving the project name from config
    /// or auto-sanitizing (replacing dots with hyphens).
    pub fn new(dir_key: &str) -> Self {
        let project_name = project_name_for_dir_key(dir_key);
        Self::with_project_name(dir_key, &project_name)
    }

    /// Create `ProjectPaths` with an explicit project name (for tests or when
    /// the project name is already known).
    pub fn with_project_name(dir_key: &str, project_name: &str) -> Self {
        Self {
            dir_key: dir_key.to_string(),
            project_name: project_name.to_string(),
            base: midtown_base_dir().join("projects").join(dir_key),
            state_base: state_dir().join("midtown").join(dir_key),
        }
    }

    /// The filesystem key (git directory name, e.g., "midtown.nosync").
    pub fn dir_key(&self) -> &str {
        &self.dir_key
    }

    /// The logical project name (e.g., "midtown").
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Base project directory: `~/.midtown/projects/<dir_key>/`.
    pub fn base_dir(&self) -> &Path {
        &self.base
    }

    /// Assets directory: `~/.midtown/projects/<dir_key>/assets/`.
    pub fn assets_dir(&self) -> PathBuf {
        self.base.join("assets")
    }

    /// Task-based worktrees directory: `~/.midtown/projects/<dir_key>/worktrees/`.
    pub fn worktrees_dir(&self) -> PathBuf {
        migrate_worktree_paths(&self.dir_key);
        self.base.join("worktrees")
    }

    /// Lead worktree path: `~/.midtown/projects/<dir_key>/worktrees/lead/`.
    pub fn lead_worktree(&self) -> PathBuf {
        self.worktrees_dir().join("lead")
    }

    /// Workflows directory: `~/.midtown/projects/<dir_key>/workflows/`.
    pub fn workflows_dir(&self) -> PathBuf {
        self.base.join("workflows")
    }

    /// Daemon state file: `~/.midtown/projects/<dir_key>/daemon-state.json`.
    pub fn daemon_state_file(&self) -> PathBuf {
        self.base.join("daemon-state.json")
    }

    /// Daemon socket: `~/.local/state/midtown/<dir_key>/daemon.sock`.
    pub fn daemon_socket(&self) -> PathBuf {
        self.state_base.join("daemon.sock")
    }

    /// Plugin daemon socket: `~/.local/state/midtown/<dir_key>/plugin-daemon.sock`.
    pub fn plugin_daemon_socket(&self) -> PathBuf {
        self.state_base.join("plugin-daemon.sock")
    }

    /// Daemon log directory: `~/.midtown/projects/<dir_key>/logs/`.
    pub fn daemon_log_dir(&self) -> PathBuf {
        self.base.join("logs")
    }

    /// Daemon log file: `~/.midtown/projects/<dir_key>/logs/daemon.log`.
    pub fn daemon_log_file(&self) -> PathBuf {
        self.daemon_log_dir().join("daemon.log")
    }

    /// Hooks log file: `~/.midtown/projects/<dir_key>/logs/hooks.log`.
    pub fn hooks_log_file(&self) -> PathBuf {
        self.daemon_log_dir().join("hooks.log")
    }

    /// Daemon PID file: `~/.midtown/projects/<dir_key>/daemon.pid`.
    pub fn daemon_pid_file(&self) -> PathBuf {
        self.base.join("daemon.pid")
    }

    /// Headless output log: `~/.midtown/projects/<dir_key>/sessions/headless-<name>.jsonl`.
    pub fn headless_output(&self, coworker_name: &str) -> PathBuf {
        self.base
            .join("sessions")
            .join(format!("headless-{}.jsonl", coworker_name))
    }

    /// Lead session file: `~/.midtown/projects/<dir_key>/lead-session-id`.
    pub fn lead_session_file(&self) -> PathBuf {
        self.base.join("lead-session-id")
    }

    /// Lead system prompt file: `~/.midtown/projects/<dir_key>/lead-system-prompt.txt`.
    pub fn lead_system_prompt_file(&self) -> PathBuf {
        self.base.join("lead-system-prompt.txt")
    }

    /// GitHub state file: `~/.midtown/projects/<dir_key>/github-state.json`.
    pub fn github_state_file(&self) -> PathBuf {
        self.base.join("github-state.json")
    }

    /// Reminders file: `~/.midtown/projects/<dir_key>/reminders.json`.
    pub fn reminders_file(&self) -> PathBuf {
        self.base.join("reminders.json")
    }

    /// Cursors directory: `~/.midtown/projects/<dir_key>/cursors/`.
    pub fn cursors_dir(&self) -> PathBuf {
        self.base.join("cursors")
    }

    /// Channel file: `~/.midtown/projects/<dir_key>/channels/<channel>/history/current.jsonl`.
    pub fn channel_file(&self, channel: &str) -> PathBuf {
        self.base
            .join("channels")
            .join(channel)
            .join("history")
            .join("current.jsonl")
    }

    /// Task list ID for this project (uses dir_key for path stability).
    pub fn task_list_id(&self) -> String {
        format!("midtown-{}", self.dir_key)
    }
}

/// Parsed YAML frontmatter from an AGENTS.md file.
#[derive(Debug, Clone, Default)]
pub struct WorkflowMetadata {
    pub name: Option<String>,
    pub description: Option<String>,
    pub states: Vec<String>,
    pub transitions: std::collections::HashMap<String, Vec<String>>,
}

/// Parse YAML frontmatter from an AGENTS.md file content.
///
/// Expects content starting with `---`, followed by YAML key-value lines,
/// and ending with `---`. Supports simple `key: value`, inline arrays
/// `key: [a, b, c]`, and multi-line mappings with indented `key: [values]`.
pub fn parse_agents_md_frontmatter(content: &str) -> Option<WorkflowMetadata> {
    let mut lines = content.lines();

    // First line must be `---`
    let first = lines.next()?.trim();
    if first != "---" {
        return None;
    }

    let mut frontmatter_lines = Vec::new();
    let mut found_end = false;
    for line in lines {
        if line.trim() == "---" {
            found_end = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !found_end {
        return None;
    }

    let mut meta = WorkflowMetadata::default();

    // Parse lines — detect top-level keys vs indented (mapping) lines
    let mut current_map_key: Option<String> = None;

    for line in &frontmatter_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check if this is an indented line (part of a mapping)
        let is_indented = line.starts_with(' ') || line.starts_with('\t');

        if is_indented {
            // Indented line belongs to current_map_key (transitions mapping)
            if let Some(ref map_key) = current_map_key
                && map_key == "transitions"
                && let Some((k, v)) = parse_key_value(trimmed)
            {
                meta.transitions.insert(k, parse_inline_array(&v));
            }
            continue;
        }

        // Top-level key: value
        current_map_key = None;
        if let Some((key, value)) = parse_key_value(trimmed) {
            match key.as_str() {
                "name" => meta.name = Some(value),
                "description" => meta.description = Some(value),
                "states" => meta.states = parse_inline_array(&value),
                "transitions" => {
                    if value.is_empty() {
                        // Multi-line mapping follows
                        current_map_key = Some("transitions".to_string());
                    } else {
                        // Unlikely but handle inline
                        current_map_key = Some("transitions".to_string());
                    }
                }
                _ => {}
            }
        }
    }

    Some(meta)
}

/// Parse a `key: value` line. Returns (key, value) with value trimmed.
fn parse_key_value(line: &str) -> Option<(String, String)> {
    let colon_pos = line.find(':')?;
    let key = line[..colon_pos].trim().to_string();
    let value = line[colon_pos + 1..].trim().to_string();
    Some((key, value))
}

/// Parse an inline array like `[a, b, c]` into a Vec<String>.
fn parse_inline_array(s: &str) -> Vec<String> {
    let trimmed = s.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        // Single value or empty
        if trimmed.is_empty() {
            return Vec::new();
        }
        return vec![trimmed.to_string()];
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    inner
        .split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// A discovered workflow directory.
#[derive(Debug, Clone)]
pub struct WorkflowInfo {
    pub name: String,
    pub dir: PathBuf,
    pub workflow_py: PathBuf,
    pub agents_md: Option<PathBuf>,
    pub metadata: Option<WorkflowMetadata>,
}

/// Discover named workflows in a workflows directory.
///
/// A valid workflow is a subdirectory containing `workflow.py`.
/// Optionally includes `AGENTS.md` path if present.
pub fn discover_workflows(workflows_dir: &Path) -> Vec<WorkflowInfo> {
    let mut workflows = Vec::new();
    if !workflows_dir.is_dir() {
        return workflows;
    }
    let Ok(entries) = fs::read_dir(workflows_dir) else {
        return workflows;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let workflow_py = path.join("workflow.py");
        if !workflow_py.exists() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let agents_md_path = path.join("AGENTS.md");
        let (agents_md, metadata) = if agents_md_path.exists() {
            let content = fs::read_to_string(&agents_md_path).unwrap_or_default();
            let meta = parse_agents_md_frontmatter(&content);
            (Some(agents_md_path), meta)
        } else {
            (None, None)
        };
        workflows.push(WorkflowInfo {
            name,
            dir: path,
            workflow_py,
            agents_md,
            metadata,
        });
    }
    workflows.sort_by(|a, b| a.name.cmp(&b.name));
    workflows
}

/// Get the state directory for midtown.
///
/// Uses `XDG_STATE_HOME` if set, otherwise `~/.local/state`.
fn state_dir() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("state")
        })
}

/// Resolve the Python SDK directory (`sdk/python`).
///
/// Checks candidates in order and returns the first that exists:
/// 1. Next to the running executable (`exe_dir/sdk/python`) — source builds
/// 2. In the source tree (`CARGO_MANIFEST_DIR/sdk/python`) — `cargo run` dev builds
/// 3. In the XDG data directory (`~/.local/share/midtown/sdk/python`) — binary installs
///
/// Falls back to the XDG data-dir path even if it doesn't exist.
pub fn resolve_python_sdk_dir() -> PathBuf {
    // Candidate 1: next to the executable
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        let candidate = exe_dir.join("sdk").join("python");
        if candidate.join("pyproject.toml").exists() {
            return candidate;
        }
    }

    // Candidate 2: source tree (baked in at compile time)
    let source_candidate = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("sdk")
        .join("python");
    if source_candidate.join("pyproject.toml").exists() {
        return source_candidate;
    }

    // Candidate 3: XDG data dir (binary installs)
    midtown_data_dir().join("sdk").join("python")
}

/// Get the data directory for midtown.
///
/// Returns `~/.local/share/midtown/`. Used for static application data
/// like bundled web-app assets that are separate from the config in `~/.midtown/`.
///
/// Uses `XDG_DATA_HOME` if set, otherwise `~/.local/share`.
pub fn midtown_data_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(override_path) = test_midtown_data_dir_override() {
        return override_path;
    }

    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        });
    data_home.join("midtown")
}

/// Get the base midtown directory.
///
/// Returns `~/.midtown/`.
pub fn midtown_base_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(override_path) = test_midtown_base_dir_override() {
        return override_path;
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
}

/// Get the projects directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/`.
///
/// This is where project-specific runtime data is stored:
/// - channels/ (per-channel directories with history, notes, cursors)
/// - logs/ (daemon logs)
/// - daemon.pid (daemon PID file)
///
/// Automatically migrates from old directory structure on first access.
pub fn projects_dir_for_repo(repo: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir().join("projects").join(repo)
}

/// Get the assets directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/assets/`.
///
/// This is where coworker-generated screenshots and videos are stored.
/// It lives outside `web-app/dist/` so it persists across rebuilds and
/// worktree recreations. Files here are served by the webserver at
/// `/api/projects/<repo>/assets/<path>`.
pub fn assets_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("assets")
}

/// Get the legacy coworkers directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/coworkers/`.
///
/// This path is only used during migration to clean up legacy worktrees.
/// All new worktrees use `worktrees_dir_for_repo()` instead.
pub(crate) fn legacy_coworkers_dir_for_repo(repo: &str) -> PathBuf {
    midtown_base_dir()
        .join("projects")
        .join(repo)
        .join("coworkers")
}

/// Get the headless session output log file for a coworker.
///
/// Returns `~/.midtown/projects/<repo>/sessions/headless-<name>.jsonl`.
///
/// This file stores all StreamEvents from a headless coworker session,
/// enabling `midtown agent show` to read recent output and providing
/// persistent debug logs.
pub fn headless_output_file(repo: &str, coworker_name: &str) -> PathBuf {
    auto_migrate(repo);
    midtown_base_dir()
        .join("projects")
        .join(repo)
        .join("sessions")
        .join(format!("headless-{}.jsonl", coworker_name))
}

/// Get the task-based worktrees directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/worktrees/`.
///
/// This is where task-based worktrees are created. Each worktree is named
/// by its branch slug (e.g., `task-42-add-auth-endpoint/`), decoupled from
/// coworker identity to enable build cache reuse across reassignment.
///
/// Automatically migrates from the old `~/.midtown/worktrees/<repo>/` layout on first access.
pub fn worktrees_dir_for_repo(repo: &str) -> PathBuf {
    migrate_worktree_paths(repo);
    midtown_base_dir()
        .join("projects")
        .join(repo)
        .join("worktrees")
}

/// Get the lead worktree path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/worktrees/lead/`.
pub fn lead_worktree_path(repo: &str) -> PathBuf {
    worktrees_dir_for_repo(repo).join("lead")
}

/// Get the lead directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/`.
///
/// Lead session data now lives in the project directory alongside other
/// project-specific state. This eliminates the separate `~/.midtown/lead/`
/// directory, so the sandbox only needs `~/.midtown/projects/{project}/`.
///
/// Automatically migrates from old directory structures on first access.
pub fn lead_dir_for_repo(repo: &str) -> PathBuf {
    migrate_lead_to_project(repo);
    projects_dir_for_repo(repo)
}

/// Get the lead system prompt file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/lead-system-prompt.txt`.
///
/// This file stores the lead's system prompt (lead-common.md + common.md)
/// so it can be re-applied when attaching to a headless lead session.
pub fn lead_system_prompt_file(repo: &str) -> PathBuf {
    lead_dir_for_repo(repo).join("lead-system-prompt.txt")
}

/// Get the Lead session ID file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/lead-session-id`.
pub fn lead_session_file_for_repo(repo: &str) -> PathBuf {
    lead_dir_for_repo(repo).join("lead-session-id")
}

/// Get the Lead session ID file path for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn lead_session_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    lead_session_file_for_repo(&repo)
}

/// Get the task list ID for a specific repository.
///
/// Returns `midtown-<repo>` which should be set as `CLAUDE_CODE_TASK_LIST_ID`
/// for all Claude sessions (Lead and coworkers) to share the same task storage.
pub fn task_list_id_for_repo(repo: &str) -> String {
    format!("midtown-{}", repo)
}

/// Get the task list ID for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn task_list_id() -> String {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    task_list_id_for_repo(&repo)
}

/// Get the active channel file path for the default channel of a repository.
///
/// Returns `~/.midtown/projects/<repo>/channels/<repo>/history/current.jsonl`.
/// The channel name matches the repo name so each project has its own default channel.
pub fn channel_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo)
        .join("channels")
        .join(repo)
        .join("history")
        .join("current.jsonl")
}

/// Get the cursors directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/cursors/`.
pub fn cursors_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("cursors")
}

/// Get the GitHub state file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/github-state.json`.
///
/// This file stores persistent GitHub-related state:
/// - PR reviewer assignments (which coworker is reviewing which PR)
pub fn github_state_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("github-state.json")
}

/// Get the reminders file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/reminders.json`.
pub fn reminders_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("reminders.json")
}

/// Get the unified daemon state file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/daemon-state.json`.
///
/// This file stores all persistent daemon state:
/// - GitHub PR reviewer assignments, review cache, pending spawns
/// - One-shot reminders
pub fn daemon_state_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("daemon-state.json")
}

/// Get the daemon socket path for a specific repository.
///
/// Returns `~/.local/state/midtown/<repo>/daemon.sock`.
///
/// This ensures each project has its own daemon, preventing coworkers
/// from different projects from being mixed.
pub fn daemon_socket_for_repo(repo: &str) -> PathBuf {
    state_dir().join("midtown").join(repo).join("daemon.sock")
}

/// Enumerate all daemon sockets across all projects.
///
/// Scans `~/.local/state/midtown/*/daemon.sock` and returns `(repo_name, socket_path)`
/// pairs for each socket that exists on disk. The socket existing doesn't guarantee
/// the daemon is running — callers should handle connection failures gracefully.
pub fn enumerate_daemon_sockets() -> Vec<(String, PathBuf)> {
    enumerate_daemon_sockets_in(&state_dir().join("midtown"))
}

fn enumerate_daemon_sockets_in(midtown_state: &Path) -> Vec<(String, PathBuf)> {
    let entries = match fs::read_dir(midtown_state) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let repo_name = entry.file_name().to_string_lossy().to_string();
            let sock = path.join("daemon.sock");
            if sock.exists() {
                Some((repo_name, sock))
            } else {
                None
            }
        })
        .collect()
}

/// Get the daemon socket path for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn daemon_socket() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_socket_for_repo(&repo)
}

/// Get the daemon PID file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/daemon.pid`.
///
/// The PID file is used to enforce singleton behavior - only one daemon
/// can run per repository. The file is locked exclusively while the
/// daemon is running.
pub fn daemon_pid_file_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("daemon.pid")
}

/// Get the daemon PID file path for the current repository.
///
/// Detects the repo name from the current git working directory.
/// Falls back to "default" if not in a git repository.
pub fn daemon_pid_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_pid_file_for_repo(&repo)
}

/// Get the daemon log directory for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/`.
/// This is where daemon stdout/stderr are redirected when daemonized.
pub fn daemon_log_dir_for_repo(repo: &str) -> PathBuf {
    projects_dir_for_repo(repo).join("logs")
}

/// Get the daemon log directory for the current repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/`.
/// This is where daemon stdout/stderr are redirected when daemonized.
pub fn daemon_log_dir() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_log_dir_for_repo(&repo)
}

/// Get the daemon log file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/daemon.log`.
/// This is where tracing output is written (via `tracing_subscriber`).
pub fn daemon_log_file_for_repo(repo: &str) -> PathBuf {
    daemon_log_dir_for_repo(repo).join("daemon.log")
}

/// Get the daemon log file path for the current repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/daemon.log`.
pub fn daemon_log_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    daemon_log_file_for_repo(&repo)
}

/// Get the hooks log file path for a specific repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/hooks.log`.
/// Hook handlers append timestamped lines here for debugging.
pub fn hooks_log_file_for_repo(repo: &str) -> PathBuf {
    daemon_log_dir_for_repo(repo).join("hooks.log")
}

/// Get the hooks log file path for the current repository.
///
/// Returns `~/.midtown/projects/<repo>/logs/hooks.log`.
pub fn hooks_log_file() -> PathBuf {
    let repo = detect_repo_name().unwrap_or_else(|| "default".to_string());
    hooks_log_file_for_repo(&repo)
}

/// Find a workflow script for a specific channel.
///
/// Implements a 4-level resolution order so scripts can be committed to the repo
/// (shared with the team) or kept local (machine-specific overrides):
///
/// AGENTS.md scope: "channel" or "project".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsMdScope {
    Channel,
    Project,
}

/// Build the candidate path list for AGENTS.md resolution.
///
/// Returns `(path, scope, source_label)` tuples in priority order:
///
/// 1. `<project_root>/.midtown/channels/<channel>/AGENTS.md` — channel-repo
/// 2. `~/.midtown/projects/<repo>/channels/<channel>/AGENTS.md` — channel-local
/// 3. `<project_root>/.midtown/AGENTS.md` — project-repo
/// 4. `~/.midtown/projects/<repo>/AGENTS.md` — project-local
pub fn agents_md_candidates(
    channel: &str,
    project_root: Option<&Path>,
    repo: &str,
) -> Vec<(std::path::PathBuf, AgentsMdScope, &'static str)> {
    let local_dir = projects_dir_for_repo(repo);
    let mut candidates = Vec::new();

    // 1. Channel-specific, in repo
    if let Some(root) = project_root {
        candidates.push((
            root.join(".midtown")
                .join("channels")
                .join(channel)
                .join("AGENTS.md"),
            AgentsMdScope::Channel,
            "channel-repo",
        ));
    }
    // 2. Channel-specific, local
    candidates.push((
        local_dir.join("channels").join(channel).join("AGENTS.md"),
        AgentsMdScope::Channel,
        "channel-local",
    ));
    // 3. Project-wide, in repo
    if let Some(root) = project_root {
        candidates.push((
            root.join(".midtown").join("AGENTS.md"),
            AgentsMdScope::Project,
            "project-repo",
        ));
    }
    // 4. Project-wide, local
    candidates.push((
        local_dir.join("AGENTS.md"),
        AgentsMdScope::Project,
        "project-local",
    ));

    candidates
}

/// Resolve the `AGENTS.md` file for a channel (first found wins).
///
/// Returns the content of the first `AGENTS.md` found, or `None` if none exist.
pub fn agents_md_for_channel(channel: &str, project_root: &Path, repo: &str) -> Option<String> {
    let candidates = agents_md_candidates(channel, Some(project_root), repo);

    for (path, _, _) in &candidates {
        if let Ok(content) = std::fs::read_to_string(path)
            && !content.trim().is_empty()
        {
            return Some(content);
        }
    }

    None
}

/// Path to the channel directory setting file for a channel.
///
/// Stored at `~/.midtown/projects/<repo>/channels/<channel>/directory`.
/// Contains a single line: the relative subdirectory within the repo
/// (e.g., "packages/auth") that the channel lead should use as its working
/// directory. When empty or absent, the channel lead uses the repo root.
pub fn channel_directory_file(repo: &str, channel: &str) -> PathBuf {
    projects_dir_for_repo(repo)
        .join("channels")
        .join(channel)
        .join("directory")
}

/// Read the channel directory setting for a channel.
///
/// Returns `Some(subdir)` if the file exists and contains a non-empty value,
/// `None` otherwise.
pub fn read_channel_directory(repo: &str, channel: &str) -> Option<String> {
    let path = channel_directory_file(repo, channel);
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the `AGENTS.md` content for a named workflow.
///
/// Looks for `<workflows_dir>/<workflow_name>/AGENTS.md` and returns its content
/// if the file exists and is non-empty.
pub fn workflow_agents_md_content(workflows_dir: &Path, workflow_name: &str) -> Option<String> {
    // Reject workflow names that could escape the workflows directory
    if workflow_name.is_empty()
        || workflow_name.contains('/')
        || workflow_name.contains('\\')
        || workflow_name.contains("..")
        || workflow_name.starts_with('.')
    {
        return None;
    }

    let path = workflows_dir.join(workflow_name).join("AGENTS.md");
    match std::fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => Some(content),
        _ => None,
    }
}

/// Merge existing `agents_md` with workflow-specific AGENTS.md and state summary.
///
/// Combines up to three pieces:
/// 1. Existing channel/project AGENTS.md (`existing`)
/// 2. Workflow-specific AGENTS.md (`workflow_agents`)
/// 3. Brief workflow state summary (`state_summary`)
///
/// Returns `None` only when all inputs are `None`.
pub fn merge_workflow_agents_md(
    existing: Option<String>,
    workflow_agents: Option<&str>,
    state_summary: Option<&str>,
) -> Option<String> {
    let has_content = existing.is_some() || workflow_agents.is_some() || state_summary.is_some();
    if !has_content {
        return None;
    }

    let mut parts = Vec::new();

    if let Some(ref e) = existing {
        parts.push(e.clone());
    }

    if let Some(wf) = workflow_agents {
        parts.push(format!("## Assigned Workflow Instructions\n\n{wf}"));
    }

    if let Some(ss) = state_summary {
        parts.push(format!("## Current Workflow State\n\n{ss}"));
    }

    Some(parts.join("\n\n"))
}

/// Get the workflow state file path for a channel (legacy).
///
/// Returns `~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json`.
///
/// **Deprecated:** Workflow state is now owned by the daemon in-memory
/// (`DaemonPersistentState::workflow_state`) and persisted to `daemon-state.json`.
/// This path is still passed to legacy `workflow.py` scripts via `--state` for
/// backward compatibility, but new plugin-based workflows should use the
/// `workflow.get_state` / `workflow.set_state` RPC methods instead.
pub fn workflow_state_file(channel: &str, repo: &str) -> PathBuf {
    projects_dir_for_repo(repo)
        .join("channels")
        .join(channel)
        .join("workflow-state.json")
}

/// Rename `tmp` to `target`, cleaning up `tmp` if the rename fails.
///
/// This wraps `fs::rename` to ensure temp files are not leaked on disk when
/// the rename step of an atomic write fails (e.g., permission denied).
pub fn atomic_rename(tmp: &Path, target: &Path) -> std::io::Result<()> {
    if let Err(e) = fs::rename(tmp, target) {
        if let Err(cleanup_err) = fs::remove_file(tmp) {
            tracing::warn!(
                "Failed to clean up temp file {} after failed rename: {}",
                tmp.display(),
                cleanup_err
            );
        }
        return Err(e);
    }
    Ok(())
}

/// Migrate data from the old directory structure to the new one (era 1).
///
/// Old structure: `~/.midtown/<repo>/...`
/// New structure: `~/.midtown/projects/<repo>/...`
///
/// Migrates:
/// - `channel.jsonl` -> `projects/<repo>/channel.jsonl`
/// - `cursors/` -> `projects/<repo>/cursors/`
/// - `logs/` -> `projects/<repo>/logs/`
/// - `daemon.pid` -> `projects/<repo>/daemon.pid`
/// - `worktrees/` -> `projects/<repo>/worktrees/`
/// - `lead-session-id` -> `projects/<repo>/lead-session-id`
/// - `lead-initialized` -> `projects/<repo>/lead-initialized`
///
/// Note: [`migrate_worktree_paths()`] (era 2) handles a separate migration
/// from `~/.midtown/projects/<repo>/coworkers/` to
/// `~/.midtown/projects/<repo>/worktrees/` for repos that were partially
/// migrated between the two eras. [`migrate_lead_to_project()`] (era 3)
/// handles migration from `~/.midtown/lead/<repo>/` into `projects/<repo>/`.
///
/// Returns Ok(true) if migration was performed, Ok(false) if already migrated or nothing to migrate.
pub fn migrate_directory_structure(repo: &str) -> std::io::Result<bool> {
    let base = midtown_base_dir();
    let old_repo_dir = base.join(repo);

    // Check if old structure exists
    if !old_repo_dir.exists() {
        return Ok(false);
    }

    // Check if already migrated (new structure exists).
    //
    // IMPORTANT: Build migration target paths directly from `base` instead of
    // calling helper accessors like `projects_dir_for_repo()`. Those helpers
    // call `auto_migrate()`, which would re-enter this function recursively.
    let new_projects_dir = base.join("projects").join(repo);
    if new_projects_dir.exists() && new_projects_dir.join("channel.jsonl").exists() {
        return Ok(false);
    }

    // Create new directories (direct paths; avoid helper recursion)
    let new_worktrees_dir = new_projects_dir.join("worktrees");
    std::fs::create_dir_all(&new_projects_dir)?;
    std::fs::create_dir_all(&new_worktrees_dir)?;

    // Migrate project files
    let project_files = [
        "channel.jsonl",
        "daemon.pid",
        "cursors",
        "logs",
        "flagged_prs",
        "insights",
    ];
    for file in &project_files {
        let old_path = old_repo_dir.join(file);
        let new_path = new_projects_dir.join(file);
        if old_path.exists() && !new_path.exists() {
            std::fs::rename(&old_path, &new_path)?;
        }
    }

    // Migrate worktrees directory (era-1 → task-based worktrees)
    let old_worktrees = old_repo_dir.join("worktrees");
    let new_worktrees = new_worktrees_dir;
    if old_worktrees.exists() {
        // Move each worktree directory, then repair git metadata
        let mut moved_paths = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&old_worktrees) {
            for entry in entries.flatten() {
                let old_path = entry.path();
                if old_path.is_dir()
                    && let Some(name) = old_path.file_name()
                {
                    let new_path = new_worktrees.join(name);
                    if !new_path.exists() {
                        std::fs::rename(&old_path, &new_path)?;
                        moved_paths.push(new_path);
                    }
                }
            }
        }
        // Repair git worktree metadata — fs::rename doesn't update git's
        // internal pointers, so worktrees would become stale without this.
        for path in &moved_paths {
            let _ = std::process::Command::new("git")
                .current_dir(path)
                .args(["worktree", "repair"])
                .output();
        }
        // Remove empty old worktrees directory
        let _ = std::fs::remove_dir(&old_worktrees);
    }

    // Migrate lead session files directly into the project directory.
    // These were previously at `<repo>/lead-session-id` and now live at
    // `projects/<repo>/lead-session-id` (no separate lead/ directory).
    let old_session_file = old_repo_dir.join("lead-session-id");
    let new_session_file = new_projects_dir.join("lead-session-id");
    if old_session_file.exists() && !new_session_file.exists() {
        std::fs::rename(&old_session_file, &new_session_file)?;
    }

    let old_initialized = old_repo_dir.join("lead-initialized");
    let new_initialized = new_projects_dir.join("lead-initialized");
    if old_initialized.exists() && !new_initialized.exists() {
        std::fs::rename(&old_initialized, &new_initialized)?;
    }

    // Try to remove old repo directory if empty
    let _ = std::fs::remove_dir(&old_repo_dir);

    Ok(true)
}

/// Migrate worktree paths from old layout to new project-grouped layout.
///
/// Old layout:
/// - `~/.midtown/worktrees/<repo>/` → `~/.midtown/projects/<repo>/worktrees/`
/// - `~/.midtown/coworkers/<repo>/` → `~/.midtown/projects/<repo>/coworkers/`
///
/// This is called internally by `worktrees_dir_for_repo()` and
/// `coworkers_dir_for_repo()` to ensure seamless migration on first access.
/// It's idempotent and only runs once per session per repo.
pub fn migrate_worktree_paths(repo: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static MIGRATED_WT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let migrated = MIGRATED_WT.get_or_init(|| Mutex::new(HashSet::new()));

    {
        let guard = migrated.lock().unwrap();
        if guard.contains(repo) {
            return;
        }
        // Don't mark as migrated yet — only mark after success so failed
        // migrations can be retried on next access.
    }

    match do_migrate_worktree_paths(repo) {
        Ok(_) => {
            let mut guard = migrated.lock().unwrap();
            guard.insert(repo.to_string());
        }
        Err(e) => {
            tracing::warn!("Failed to migrate worktree paths for {}: {}", repo, e);
        }
    }
}

/// Perform the actual migration of worktree paths.
///
/// Returns `Ok(true)` if any migration was performed, `Ok(false)` if nothing to migrate.
pub fn do_migrate_worktree_paths(repo: &str) -> std::io::Result<bool> {
    let base = midtown_base_dir();
    let projects_dir = base.join("projects").join(repo);
    let mut migrated_any = false;

    // Migrate ~/.midtown/worktrees/<repo>/ → ~/.midtown/projects/<repo>/worktrees/
    let old_worktrees = base.join("worktrees").join(repo);
    let new_worktrees = projects_dir.join("worktrees");
    if old_worktrees.exists() && !new_worktrees.exists() {
        fs::create_dir_all(&projects_dir)?;
        fs::rename(&old_worktrees, &new_worktrees)?;
        tracing::info!(
            "Migrated worktrees: {} -> {}",
            old_worktrees.display(),
            new_worktrees.display()
        );
        migrated_any = true;

        // Clean up empty parent ~/.midtown/worktrees/ if it's now empty
        let old_worktrees_parent = base.join("worktrees");
        let _ = fs::remove_dir(&old_worktrees_parent);
    }

    // Migrate ~/.midtown/coworkers/<repo>/ → ~/.midtown/projects/<repo>/coworkers/
    let old_coworkers = base.join("coworkers").join(repo);
    let new_coworkers = projects_dir.join("coworkers");
    if old_coworkers.exists() && !new_coworkers.exists() {
        fs::create_dir_all(&projects_dir)?;
        fs::rename(&old_coworkers, &new_coworkers)?;
        tracing::info!(
            "Migrated coworkers: {} -> {}",
            old_coworkers.display(),
            new_coworkers.display()
        );
        migrated_any = true;

        // Clean up empty parent ~/.midtown/coworkers/ if it's now empty
        let old_coworkers_parent = base.join("coworkers");
        let _ = fs::remove_dir(&old_coworkers_parent);
    }

    Ok(migrated_any)
}

/// Migrate lead session data from `~/.midtown/lead/<repo>/` into
/// `~/.midtown/projects/<repo>/` (era 3).
///
/// Moves known files:
/// - `lead/<repo>/session-id` → `projects/<repo>/lead-session-id`
/// - `lead/<repo>/system-prompt.txt` → `projects/<repo>/lead-system-prompt.txt`
/// - `lead/<repo>/lead-initialized` → `projects/<repo>/lead-initialized`
///
/// Idempotent — only runs once per session per repo.
pub fn migrate_lead_to_project(repo: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static MIGRATED_LEAD: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let migrated = MIGRATED_LEAD.get_or_init(|| Mutex::new(HashSet::new()));

    {
        let guard = migrated.lock().unwrap();
        if guard.contains(repo) {
            return;
        }
        // Don't mark as migrated yet — only mark after success so failed
        // migrations can be retried on next access.
    }

    match do_migrate_lead_to_project(repo) {
        Ok(_) => {
            let mut guard = migrated.lock().unwrap();
            guard.insert(repo.to_string());
        }
        Err(e) => {
            tracing::warn!("Failed to migrate lead data for {}: {}", repo, e);
        }
    }
}

/// Perform the actual migration of lead data into the project directory.
///
/// Returns `Ok(true)` if any migration was performed, `Ok(false)` if nothing to migrate.
pub fn do_migrate_lead_to_project(repo: &str) -> std::io::Result<bool> {
    let base = midtown_base_dir();
    let old_lead_dir = base.join("lead").join(repo);

    if !old_lead_dir.exists() {
        return Ok(false);
    }

    // Ensure target project directory exists (avoid calling projects_dir_for_repo
    // which would recurse through auto_migrate).
    let projects_dir = base.join("projects").join(repo);
    fs::create_dir_all(&projects_dir)?;

    let mut migrated_any = false;

    // Migrate known files with lead- prefix
    let migrations = [
        ("session-id", "lead-session-id"),
        ("system-prompt.txt", "lead-system-prompt.txt"),
        ("lead-initialized", "lead-initialized"),
    ];

    for (old_name, new_name) in &migrations {
        let old_path = old_lead_dir.join(old_name);
        let new_path = projects_dir.join(new_name);
        if old_path.exists() && !new_path.exists() {
            fs::rename(&old_path, &new_path)?;
            tracing::info!(
                "Migrated lead file: {} -> {}",
                old_path.display(),
                new_path.display()
            );
            migrated_any = true;
        }
    }

    // Clean up empty lead directory and parent
    let _ = fs::remove_dir(&old_lead_dir);
    let lead_parent = base.join("lead");
    let _ = fs::remove_dir(&lead_parent);

    Ok(migrated_any)
}

/// Auto-migrate on first access.
///
/// This is called internally when accessing paths to ensure seamless migration.
/// It's idempotent and only runs once per session per repo.
pub fn auto_migrate(repo: &str) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::OnceLock;

    static MIGRATED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let migrated = MIGRATED.get_or_init(|| Mutex::new(HashSet::new()));

    {
        let guard = migrated.lock().unwrap();
        if guard.contains(repo) {
            return;
        }
        // Don't mark as migrated yet — only mark after success so failed
        // migrations can be retried on next access.
    }

    // Attempt migrations silently
    let _ = migrate_directory_structure(repo);
    if let Err(e) = migrate_legacy_coworker_worktrees(repo) {
        tracing::warn!(
            "Failed to migrate legacy coworker worktrees for {}: {}",
            repo,
            e
        );
        return; // Don't mark as migrated — allow retry
    }
    let _ = migrate_headless_transcripts_to_sessions(repo);

    let mut guard = migrated.lock().unwrap();
    guard.insert(repo.to_string());
}

/// Migrate coworker-named worktrees from `~/.midtown/projects/<repo>/coworkers/`
/// to the unified `~/.midtown/projects/<repo>/worktrees/` layout.
///
/// This handles the second migration era: the `coworkers/` directory was an
/// intermediate layout that predates task-based worktrees. Each coworker had
/// a worktree at `projects/<repo>/coworkers/<coworker-name>/`. These are
/// moved to `projects/<repo>/worktrees/<coworker-name>/`.
///
/// Empty `coworkers/` directories are cleaned up after migration.
fn migrate_legacy_coworker_worktrees(repo: &str) -> std::io::Result<bool> {
    let old_coworkers_dir = legacy_coworkers_dir_for_repo(repo);

    if !old_coworkers_dir.exists() {
        return Ok(false);
    }

    let new_worktrees_dir = worktrees_dir_for_repo(repo);
    std::fs::create_dir_all(&new_worktrees_dir)?;

    let mut migrated_paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&old_coworkers_dir) {
        for entry in entries.flatten() {
            let old_path = entry.path();
            if old_path.is_dir()
                && let Some(name) = old_path.file_name()
            {
                let new_path = new_worktrees_dir.join(name);
                if !new_path.exists() {
                    match std::fs::rename(&old_path, &new_path) {
                        Ok(()) => migrated_paths.push(new_path),
                        Err(e) => tracing::warn!(
                            "Failed to migrate coworker worktree {}: {}",
                            old_path.display(),
                            e
                        ),
                    }
                }
            }
        }
    }

    let migrated_any = !migrated_paths.is_empty();

    // Repair git worktree metadata after moving directories.
    // fs::rename doesn't update git's internal pointers, so without this,
    // `git worktree prune` could remove the worktree metadata and make
    // the moved worktrees unusable.
    for path in &migrated_paths {
        let _ = std::process::Command::new("git")
            .current_dir(path)
            .args(["worktree", "repair"])
            .output();
    }

    // Clean up empty directories
    let _ = std::fs::remove_dir(&old_coworkers_dir);
    let coworkers_parent = midtown_base_dir().join("coworkers");
    let _ = std::fs::remove_dir(&coworkers_parent);

    Ok(migrated_any)
}

/// Migrate `headless-*.jsonl` transcript files from the project root
/// (`~/.midtown/projects/<repo>/`) into the `sessions/` subdirectory.
///
/// Old files are moved (renamed) into `sessions/`. Files that already
/// exist at the new path are skipped to avoid overwriting active transcripts.
fn migrate_headless_transcripts_to_sessions(repo: &str) -> std::io::Result<bool> {
    let project_dir = midtown_base_dir().join("projects").join(repo);
    let sessions_dir = project_dir.join("sessions");

    let entries = match std::fs::read_dir(&project_dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(false),
    };

    let mut migrated_any = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("headless-") && name_str.ends_with(".jsonl") {
            // Create sessions/ on first match (avoid creating empty dirs)
            if !migrated_any {
                std::fs::create_dir_all(&sessions_dir)?;
            }
            let new_path = sessions_dir.join(&name);
            if !new_path.exists() {
                if let Err(e) = std::fs::rename(entry.path(), &new_path) {
                    tracing::warn!("Failed to migrate transcript {}: {}", name_str, e);
                } else {
                    migrated_any = true;
                }
            }
        }
    }

    Ok(migrated_any)
}

#[path = "paths_tests.rs"]
#[cfg(test)]
mod tests;
