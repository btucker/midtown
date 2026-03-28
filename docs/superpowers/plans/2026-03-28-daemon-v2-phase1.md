# Daemon v2 Phase 1: Event Store, Projections, and RPC Skeleton

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational layer of the v2 daemon — event store with snapshot/recovery, three projection types (AgentIndex, WorkIndex, ChannelIndex), cooldown tracker, and a minimal RPC server that responds to `status` and `agent.list`. The v1 daemon continues to run; v2 is a new module compiled alongside it.

**Architecture:** Event-sourced core where every state mutation flows through `Command → execute → DomainEvent → apply to projections`. Events are appended to a JSONL log file. Projections are materialized views rebuilt from events. Snapshots checkpoint projection state periodically. The RPC server uses the same Unix socket + JSON-RPC 2.0 protocol as v1.

**Tech Stack:** Rust, tokio, serde/serde_json, chrono, uuid

**Spec:** `docs/superpowers/specs/2026-03-28-daemon-v2-design.md`

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/daemon_v2/mod.rs` | Create | Module root — re-exports, `run()` entry point |
| `src/daemon_v2/events/mod.rs` | Create | `DomainEvent` enum definition |
| `src/daemon_v2/events/store.rs` | Create | Append-only JSONL log, snapshot, recovery |
| `src/daemon_v2/events/store_tests.rs` | Create | Tests for event store |
| `src/daemon_v2/projections/mod.rs` | Create | `Projections` container, `apply()` dispatch |
| `src/daemon_v2/projections/agents.rs` | Create | `AgentIndex` projection |
| `src/daemon_v2/projections/agents_tests.rs` | Create | Tests for AgentIndex |
| `src/daemon_v2/projections/work.rs` | Create | `WorkIndex` projection (tasks + PRs) |
| `src/daemon_v2/projections/work_tests.rs` | Create | Tests for WorkIndex |
| `src/daemon_v2/projections/channels.rs` | Create | `ChannelIndex` projection |
| `src/daemon_v2/projections/channels_tests.rs` | Create | Tests for ChannelIndex |
| `src/daemon_v2/projections/cooldowns.rs` | Create | `CooldownTracker` |
| `src/daemon_v2/projections/cooldowns_tests.rs` | Create | Tests for CooldownTracker |
| `src/daemon_v2/rpc/mod.rs` | Create | RPC server setup, method dispatch |
| `src/daemon_v2/rpc/handlers.rs` | Create | `status` and `agent.list` handlers |
| `src/daemon_v2/rpc/rpc_tests.rs` | Create | Tests for RPC handlers |
| `src/lib.rs` | Modify | Add `pub mod daemon_v2;` |

---

### Task 1: Module skeleton and DomainEvent enum

**Files:**
- Create: `src/daemon_v2/mod.rs`
- Create: `src/daemon_v2/events/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Create the module directory structure**

Run:
```bash
mkdir -p src/daemon_v2/events src/daemon_v2/projections src/daemon_v2/rpc
```

- [ ] **Step 2: Add the daemon_v2 module to lib.rs**

In `src/lib.rs`, find the line `pub mod daemon;` and add below it:

```rust
pub mod daemon_v2;
```

- [ ] **Step 3: Create src/daemon_v2/mod.rs**

```rust
pub mod events;
pub mod projections;

pub use events::DomainEvent;
pub use projections::Projections;
```

- [ ] **Step 4: Create src/daemon_v2/events/mod.rs with DomainEvent**

```rust
mod store;

pub use store::EventStore;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type AgentId = String;
pub type TaskId = String;
pub type MessageId = String;
pub type WorktreeId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Lead,
    Fork,
    Worker,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    ClaudeCode,
    Codex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiStatus {
    Pending,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewState {
    None,
    Pending,
    Approved,
    ChangesRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessStatus {
    Running,
    Stopped,
    Dead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DomainEvent {
    // Agents
    AgentCreated {
        id: AgentId,
        name: String,
        kind: AgentKind,
        agent_type: String,
        provider: Provider,
        channel: Option<String>,
        task_id: Option<TaskId>,
    },
    AgentStarted {
        id: AgentId,
        pid: u32,
    },
    AgentStopped {
        id: AgentId,
        reason: String,
    },
    AgentResumed {
        id: AgentId,
    },

    // Tasks
    TaskCreated {
        id: TaskId,
        subject: String,
        channel: String,
        blocked_by: Vec<TaskId>,
    },
    TaskAssigned {
        task_id: TaskId,
        agent_id: AgentId,
    },
    TaskCompleted {
        task_id: TaskId,
    },
    TaskReset {
        task_id: TaskId,
        reason: String,
    },
    TaskUnblocked {
        task_id: TaskId,
    },

    // PRs
    PrOpened {
        number: u64,
        branch: String,
        author: String,
    },
    PrUpdated {
        number: u64,
        ci_status: CiStatus,
        review_state: ReviewState,
    },
    PrMerged {
        number: u64,
        branch: String,
    },
    PrClosed {
        number: u64,
    },
    PrReviewRequested {
        number: u64,
    },
    PrLinkedToTask {
        number: u64,
        task_id: TaskId,
    },

    // Chat
    MessagePosted {
        id: MessageId,
        channel: String,
        sender: String,
        content: String,
        thread_id: Option<String>,
    },
    MentionRouted {
        message_id: MessageId,
        target_agent: AgentId,
    },

    // Health
    ProcessHealthChecked {
        agent_id: AgentId,
        status: ProcessStatus,
    },
    UsageLimitHit {
        agent_id: AgentId,
        reset_at: DateTime<Utc>,
    },
    AuthErrorDetected {
        agent_id: AgentId,
    },

    // Worktrees
    WorktreeCreated {
        id: WorktreeId,
        path: PathBuf,
        task_id: Option<TaskId>,
    },
    WorktreeRemoved {
        id: WorktreeId,
    },

    // Config
    ConfigUpdated {
        key: String,
        value: serde_json::Value,
    },
}
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles successfully (store module is declared but empty — we'll fill it next).

- [ ] **Step 6: Commit**

```bash
git add src/daemon_v2/ src/lib.rs
git commit -m "feat(daemon-v2): add module skeleton and DomainEvent enum"
```

---

### Task 2: Event store — append, read, snapshot, recovery

**Files:**
- Create: `src/daemon_v2/events/store.rs`
- Create: `src/daemon_v2/events/store_tests.rs`

- [ ] **Step 1: Create src/daemon_v2/events/store_tests.rs with failing tests**

```rust
use super::*;
use tempfile::TempDir;

fn temp_store() -> (EventStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = EventStore::new(dir.path().to_path_buf());
    (store, dir)
}

fn sample_agent_created() -> DomainEvent {
    DomainEvent::AgentCreated {
        id: "agent-1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
    }
}

fn sample_task_created() -> DomainEvent {
    DomainEvent::TaskCreated {
        id: "task-1".into(),
        subject: "Fix auth bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    }
}

#[test]
fn append_and_read_back() {
    let (mut store, _dir) = temp_store();
    let event = sample_agent_created();

    store.append(&event).unwrap();
    store.append(&sample_task_created()).unwrap();

    let events = store.events_since(0).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(store.sequence(), 2);
}

#[test]
fn snapshot_and_recover() {
    let (mut store, dir) = temp_store();

    // Append 3 events
    store.append(&sample_agent_created()).unwrap();
    store.append(&sample_task_created()).unwrap();
    store.append(&DomainEvent::AgentStarted {
        id: "agent-1".into(),
        pid: 1234,
    }).unwrap();

    assert_eq!(store.sequence(), 3);

    // Take snapshot at current sequence
    let projections = Projections::default();
    store.save_snapshot(&projections).unwrap();

    // Append one more event after snapshot
    store.append(&DomainEvent::TaskCompleted {
        task_id: "task-1".into(),
    }).unwrap();

    assert_eq!(store.sequence(), 4);

    // Recover from disk — should load snapshot + replay 1 event
    let (recovered_store, snapshot, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(recovered_store.sequence(), 4);
    assert!(snapshot.is_some());
    assert_eq!(replay_events.len(), 1);
}

#[test]
fn recover_empty_directory() {
    let dir = TempDir::new().unwrap();
    let (store, snapshot, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(store.sequence(), 0);
    assert!(snapshot.is_none());
    assert_eq!(replay_events.len(), 0);
}

#[test]
fn truncates_partial_line_on_recovery() {
    let (mut store, dir) = temp_store();
    store.append(&sample_agent_created()).unwrap();
    drop(store);

    // Simulate crash: append partial JSON to the log file
    let log_path = dir.path().join("log-0000.jsonl");
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    write!(f, "{{\"broken\":").unwrap();

    // Recovery should ignore the partial line
    let (recovered_store, _, replay_events) =
        EventStore::recover(dir.path().to_path_buf()).unwrap();

    assert_eq!(recovered_store.sequence(), 1);
    assert_eq!(replay_events.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon_v2::events::store::tests 2>&1 | tail -10`
Expected: Compilation errors — `EventStore` and its methods don't exist yet.

- [ ] **Step 3: Create src/daemon_v2/events/store.rs**

```rust
use super::DomainEvent;
use crate::daemon_v2::Projections;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

#[path = "store_tests.rs"]
#[cfg(test)]
mod tests;

pub struct EventStore {
    dir: PathBuf,
    sequence: u64,
    snapshot_sequence: u64,
    writer: Option<io::BufWriter<fs::File>>,
}

impl EventStore {
    pub fn new(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).expect("failed to create event store directory");
        let log_path = dir.join("log-0000.jsonl");
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("failed to open event log");

        Self {
            dir,
            sequence: 0,
            snapshot_sequence: 0,
            writer: Some(io::BufWriter::new(file)),
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn append(&mut self, event: &DomainEvent) -> io::Result<()> {
        let writer = self.writer.as_mut().expect("event store not open");
        let json = serde_json::to_string(event)?;
        writeln!(writer, "{json}")?;
        writer.flush()?;
        self.sequence += 1;
        Ok(())
    }

    pub fn events_since(&self, since_sequence: u64) -> io::Result<Vec<DomainEvent>> {
        let log_path = self.log_path_for_snapshot(self.snapshot_sequence);
        if !log_path.exists() {
            return Ok(vec![]);
        }

        let file = fs::File::open(&log_path)?;
        let reader = io::BufReader::new(file);
        let mut events = Vec::new();

        for (i, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let absolute_seq = self.snapshot_sequence + i as u64;
            if absolute_seq < since_sequence {
                continue;
            }
            match serde_json::from_str::<DomainEvent>(&line) {
                Ok(event) => events.push(event),
                Err(_) => break, // partial line — stop reading
            }
        }

        Ok(events)
    }

    pub fn save_snapshot(&mut self, projections: &Projections) -> io::Result<()> {
        let snapshot_path = self.dir.join(format!("snapshot-{:04}.json", self.sequence));
        let json = serde_json::to_string_pretty(projections)?;
        fs::write(&snapshot_path, json)?;
        self.snapshot_sequence = self.sequence;

        // Start a new log file
        drop(self.writer.take());
        let log_path = self.log_path_for_snapshot(self.sequence);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        self.writer = Some(io::BufWriter::new(file));

        Ok(())
    }

    pub fn recover(dir: PathBuf) -> io::Result<(Self, Option<Projections>, Vec<DomainEvent>)> {
        if !dir.exists() {
            fs::create_dir_all(&dir)?;
            let store = Self::new(dir);
            return Ok((store, None, vec![]));
        }

        // Find latest snapshot
        let (snapshot, snapshot_seq) = Self::load_latest_snapshot(&dir)?;

        // Read events from the log file for this snapshot
        let log_path = dir.join(format!("log-{snapshot_seq:04}.jsonl"));
        let mut events = Vec::new();

        if log_path.exists() {
            let contents = fs::read_to_string(&log_path)?;
            for line in contents.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<DomainEvent>(line) {
                    Ok(event) => events.push(event),
                    Err(_) => break, // partial line — truncate
                }
            }

            // Rewrite log without partial trailing line
            let mut file = fs::File::create(&log_path)?;
            for event in &events {
                let json = serde_json::to_string(event)?;
                writeln!(file, "{json}")?;
            }
        }

        let total_sequence = snapshot_seq + events.len() as u64;

        // Open log file for appending
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        let store = Self {
            dir,
            sequence: total_sequence,
            snapshot_sequence: snapshot_seq,
            writer: Some(io::BufWriter::new(file)),
        };

        Ok((store, snapshot, events))
    }

    fn load_latest_snapshot(dir: &PathBuf) -> io::Result<(Option<Projections>, u64)> {
        let mut entries: Vec<_> = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("snapshot-") && n.ends_with(".json"))
                    .unwrap_or(false)
            })
            .collect();

        entries.sort_by_key(|e| e.file_name());

        if let Some(latest) = entries.last() {
            let name = latest.file_name();
            let name_str = name.to_str().unwrap_or("snapshot-0000.json");
            let seq_str = name_str
                .strip_prefix("snapshot-")
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap_or("0000");
            let seq: u64 = seq_str.parse().unwrap_or(0);

            let content = fs::read_to_string(latest.path())?;
            let projections: Projections = serde_json::from_str(&content)?;
            Ok((Some(projections), seq))
        } else {
            Ok((None, 0))
        }
    }

    fn log_path_for_snapshot(&self, snapshot_seq: u64) -> PathBuf {
        self.dir.join(format!("log-{snapshot_seq:04}.jsonl"))
    }
}
```

- [ ] **Step 4: Create empty Projections for compilation**

We need `Projections` to exist for EventStore to compile. In `src/daemon_v2/projections/mod.rs`:

```rust
use serde::{Deserialize, Serialize};
use super::events::DomainEvent;

pub mod agents;
pub mod work;
pub mod channels;
pub mod cooldowns;

pub use agents::AgentIndex;
pub use work::WorkIndex;
pub use channels::ChannelIndex;
pub use cooldowns::CooldownTracker;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Projections {
    pub agents: AgentIndex,
    pub work: WorkIndex,
    pub channels: ChannelIndex,
    #[serde(skip)]
    pub cooldowns: CooldownTracker,
}

impl Projections {
    pub fn apply(&mut self, event: &DomainEvent) {
        self.agents.apply(event);
        self.work.apply(event);
        self.channels.apply(event);
    }
}
```

Create stub files for each projection (we'll flesh them out in later tasks):

`src/daemon_v2/projections/agents.rs`:
```rust
use serde::{Deserialize, Serialize};
use crate::daemon_v2::events::DomainEvent;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AgentIndex {}

impl AgentIndex {
    pub fn apply(&mut self, _event: &DomainEvent) {}
}
```

`src/daemon_v2/projections/work.rs`:
```rust
use serde::{Deserialize, Serialize};
use crate::daemon_v2::events::DomainEvent;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorkIndex {}

impl WorkIndex {
    pub fn apply(&mut self, _event: &DomainEvent) {}
}
```

`src/daemon_v2/projections/channels.rs`:
```rust
use serde::{Deserialize, Serialize};
use crate::daemon_v2::events::DomainEvent;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ChannelIndex {}

impl ChannelIndex {
    pub fn apply(&mut self, _event: &DomainEvent) {}
}
```

`src/daemon_v2/projections/cooldowns.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Default)]
pub struct CooldownTracker {}

// Serde impls needed for Projections derive, but cooldowns aren't persisted
impl Serialize for CooldownTracker {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        s.serialize_struct("CooldownTracker", 0)?.end()
    }
}

impl<'de> Deserialize<'de> for CooldownTracker {
    fn deserialize<D: serde::Deserializer<'de>>(_d: D) -> Result<Self, D::Error> {
        Ok(Self::default())
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib daemon_v2::events::store::tests 2>&1 | tail -15`
Expected: All 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/daemon_v2/
git commit -m "feat(daemon-v2): add event store with append, snapshot, and crash-safe recovery"
```

---

### Task 3: AgentIndex projection

**Files:**
- Modify: `src/daemon_v2/projections/agents.rs`
- Create: `src/daemon_v2/projections/agents_tests.rs`

- [ ] **Step 1: Write tests in src/daemon_v2/projections/agents_tests.rs**

```rust
use super::*;
use crate::daemon_v2::events::*;

fn created_event(id: &str, name: &str, kind: AgentKind) -> DomainEvent {
    DomainEvent::AgentCreated {
        id: id.into(),
        name: name.into(),
        kind,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
    }
}

#[test]
fn create_and_lookup_by_id() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));

    let agent = idx.by_id.get("a1").unwrap();
    assert_eq!(agent.name, "ghost-town");
    assert_eq!(agent.kind, AgentKind::Worker);
}

#[test]
fn lookup_by_name() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));

    assert_eq!(idx.by_name.get("ghost-town"), Some(&"a1".to_string()));
}

#[test]
fn lookup_by_channel() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "lead-1", AgentKind::Lead));
    idx.apply(&created_event("a2", "worker-1", AgentKind::Worker));

    let channel_agents = idx.by_channel.get("main").unwrap();
    assert_eq!(channel_agents.len(), 2);
}

#[test]
fn started_adds_to_running() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted { id: "a1".into(), pid: 1234 });

    assert!(idx.running.contains("a1"));
    assert_eq!(idx.by_id.get("a1").unwrap().pid, Some(1234));
}

#[test]
fn stopped_removes_from_running() {
    let mut idx = AgentIndex::default();
    idx.apply(&created_event("a1", "ghost-town", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted { id: "a1".into(), pid: 1234 });
    idx.apply(&DomainEvent::AgentStopped { id: "a1".into(), reason: "completed".into() });

    assert!(!idx.running.contains("a1"));
    assert!(idx.by_id.get("a1").unwrap().stopped_at.is_some());
}

#[test]
fn lookup_by_task() {
    let mut idx = AgentIndex::default();
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
    });

    assert_eq!(idx.by_task.get("task-1"), Some(&"a1".to_string()));
}

#[test]
fn idle_workers_returns_running_workers_without_tasks() {
    let mut idx = AgentIndex::default();
    // Worker with task
    idx.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "busy".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: None,
        task_id: Some("task-1".into()),
    });
    idx.apply(&DomainEvent::AgentStarted { id: "a1".into(), pid: 1 });

    // Worker without task
    idx.apply(&created_event("a2", "idle", AgentKind::Worker));
    idx.apply(&DomainEvent::AgentStarted { id: "a2".into(), pid: 2 });

    // Lead (should not be returned)
    idx.apply(&created_event("a3", "lead", AgentKind::Lead));
    idx.apply(&DomainEvent::AgentStarted { id: "a3".into(), pid: 3 });

    let idle = idx.idle_workers();
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0], "a2");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon_v2::projections::agents::tests 2>&1 | tail -10`
Expected: Compilation errors — Agent struct and AgentIndex fields don't exist yet.

- [ ] **Step 3: Implement AgentIndex in src/daemon_v2/projections/agents.rs**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::daemon_v2::events::{AgentId, AgentKind, DomainEvent, Provider, TaskId};

#[path = "agents_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    pub kind: AgentKind,
    pub agent_type: String,
    pub provider: Provider,
    pub channel: Option<String>,
    pub task_id: Option<TaskId>,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AgentIndex {
    pub by_id: HashMap<AgentId, Agent>,
    pub by_name: HashMap<String, AgentId>,
    pub by_task: HashMap<TaskId, AgentId>,
    pub by_channel: HashMap<String, Vec<AgentId>>,
    pub running: HashSet<AgentId>,
}

impl AgentIndex {
    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::AgentCreated {
                id,
                name,
                kind,
                agent_type,
                provider,
                channel,
                task_id,
            } => {
                let agent = Agent {
                    id: id.clone(),
                    name: name.clone(),
                    kind: kind.clone(),
                    agent_type: agent_type.clone(),
                    provider: provider.clone(),
                    channel: channel.clone(),
                    task_id: task_id.clone(),
                    pid: None,
                    started_at: None,
                    stopped_at: None,
                };
                self.by_name.insert(name.clone(), id.clone());
                if let Some(task_id) = task_id {
                    self.by_task.insert(task_id.clone(), id.clone());
                }
                if let Some(channel) = channel {
                    self.by_channel
                        .entry(channel.clone())
                        .or_default()
                        .push(id.clone());
                }
                self.by_id.insert(id.clone(), agent);
            }
            DomainEvent::AgentStarted { id, pid } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = Some(*pid);
                    agent.started_at = Some(Utc::now());
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            DomainEvent::AgentStopped { id, .. } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.pid = None;
                    agent.stopped_at = Some(Utc::now());
                    self.running.remove(id);
                }
            }
            DomainEvent::AgentResumed { id } => {
                if let Some(agent) = self.by_id.get_mut(id) {
                    agent.stopped_at = None;
                    self.running.insert(id.clone());
                }
            }
            _ => {}
        }
    }

    pub fn idle_workers(&self) -> Vec<AgentId> {
        self.running
            .iter()
            .filter(|id| {
                self.by_id.get(*id).map_or(false, |a| {
                    a.kind == AgentKind::Worker && a.task_id.is_none()
                })
            })
            .cloned()
            .collect()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib daemon_v2::projections::agents::tests 2>&1 | tail -15`
Expected: All 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/projections/agents.rs src/daemon_v2/projections/agents_tests.rs
git commit -m "feat(daemon-v2): implement AgentIndex projection with indexed lookups"
```

---

### Task 4: WorkIndex projection

**Files:**
- Modify: `src/daemon_v2/projections/work.rs`
- Create: `src/daemon_v2/projections/work_tests.rs`

- [ ] **Step 1: Write tests in src/daemon_v2/projections/work_tests.rs**

```rust
use super::*;
use crate::daemon_v2::events::*;

#[test]
fn create_task_adds_to_pending() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });

    assert_eq!(idx.pending_tasks.len(), 1);
    assert_eq!(idx.tasks.get("t1").unwrap().status, TaskStatus::Pending);
}

#[test]
fn task_assigned_moves_to_in_progress() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });

    assert!(idx.pending_tasks.is_empty());
    assert_eq!(idx.in_progress_tasks.len(), 1);
    assert_eq!(idx.tasks.get("t1").unwrap().status, TaskStatus::InProgress);
}

#[test]
fn task_completed_removes_from_in_progress() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    idx.apply(&DomainEvent::TaskCompleted { task_id: "t1".into() });

    assert!(idx.in_progress_tasks.is_empty());
    assert_eq!(idx.tasks.get("t1").unwrap().status, TaskStatus::Completed);
}

#[test]
fn task_reset_returns_to_pending() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });
    idx.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    });
    idx.apply(&DomainEvent::TaskReset {
        task_id: "t1".into(),
        reason: "agent died".into(),
    });

    assert_eq!(idx.pending_tasks.len(), 1);
    assert!(idx.in_progress_tasks.is_empty());
}

#[test]
fn blocked_tasks_tracked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "First".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });
    idx.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Second".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
    });

    assert!(idx.blocked.contains_key("t2"));
    let unblocked = idx.pending_unblocked();
    assert_eq!(unblocked.len(), 1);
    assert_eq!(*unblocked[0], "t1");
}

#[test]
fn unblock_removes_from_blocked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "First".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });
    idx.apply(&DomainEvent::TaskCreated {
        id: "t2".into(),
        subject: "Second".into(),
        channel: "main".into(),
        blocked_by: vec!["t1".into()],
    });
    idx.apply(&DomainEvent::TaskUnblocked { task_id: "t2".into() });

    assert!(!idx.blocked.contains_key("t2"));
    assert_eq!(idx.pending_unblocked().len(), 2);
}

#[test]
fn pr_linked_to_task() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    });
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrLinkedToTask {
        number: 42,
        task_id: "t1".into(),
    });

    assert_eq!(idx.pr_for_task(&"t1".into()).unwrap().number, 42);
    let (task_id, _) = idx.task_for_pr(42).unwrap();
    assert_eq!(task_id, "t1");
}

#[test]
fn pr_merged_tracked() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrMerged {
        number: 42,
        branch: "fix-bug".into(),
    });

    assert!(idx.prs.get(&42).unwrap().is_merged);
    assert!(!idx.open_prs.contains(&42));
}

#[test]
fn pr_needing_review() {
    let mut idx = WorkIndex::default();
    idx.apply(&DomainEvent::PrOpened {
        number: 42,
        branch: "fix-bug".into(),
        author: "dev".into(),
    });
    idx.apply(&DomainEvent::PrReviewRequested { number: 42 });

    assert!(idx.needing_review.contains(&42));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon_v2::projections::work::tests 2>&1 | tail -10`
Expected: Compilation errors — Task, PrState structs and WorkIndex fields don't exist.

- [ ] **Step 3: Implement WorkIndex in src/daemon_v2/projections/work.rs**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::daemon_v2::events::{
    CiStatus, DomainEvent, ReviewState, TaskId, TaskStatus,
};

#[path = "work_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub subject: String,
    pub channel: String,
    pub status: TaskStatus,
    pub pr_number: Option<u64>,
    pub blocked_by: Vec<TaskId>,
    pub agent_type: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrState {
    pub number: u64,
    pub branch: String,
    pub author: String,
    pub ci_status: CiStatus,
    pub review_state: ReviewState,
    pub is_merged: bool,
    pub is_closed: bool,
    pub needs_review: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct WorkIndex {
    pub tasks: HashMap<TaskId, Task>,
    pub prs: HashMap<u64, PrState>,
    pub pending_tasks: Vec<TaskId>,
    pub in_progress_tasks: Vec<TaskId>,
    pub open_prs: Vec<u64>,
    pub needing_review: Vec<u64>,
    pub blocked: HashMap<TaskId, Vec<TaskId>>,
}

impl WorkIndex {
    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::TaskCreated {
                id,
                subject,
                channel,
                blocked_by,
            } => {
                let task = Task {
                    id: id.clone(),
                    subject: subject.clone(),
                    channel: channel.clone(),
                    status: TaskStatus::Pending,
                    pr_number: None,
                    blocked_by: blocked_by.clone(),
                    agent_type: None,
                    created_at: Utc::now(),
                    completed_at: None,
                };
                self.tasks.insert(id.clone(), task);
                if blocked_by.is_empty() {
                    self.pending_tasks.push(id.clone());
                } else {
                    self.blocked.insert(id.clone(), blocked_by.clone());
                    // Still pending, just blocked
                    self.pending_tasks.push(id.clone());
                }
            }
            DomainEvent::TaskAssigned { task_id, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::InProgress;
                    self.pending_tasks.retain(|id| id != task_id);
                    self.in_progress_tasks.push(task_id.clone());
                }
            }
            DomainEvent::TaskCompleted { task_id } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Completed;
                    task.completed_at = Some(Utc::now());
                    self.in_progress_tasks.retain(|id| id != task_id);
                }
            }
            DomainEvent::TaskReset { task_id, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Pending;
                    self.in_progress_tasks.retain(|id| id != task_id);
                    if !self.pending_tasks.contains(task_id) {
                        self.pending_tasks.push(task_id.clone());
                    }
                }
            }
            DomainEvent::TaskUnblocked { task_id } => {
                self.blocked.remove(task_id);
            }
            DomainEvent::PrOpened {
                number,
                branch,
                author,
            } => {
                let pr = PrState {
                    number: *number,
                    branch: branch.clone(),
                    author: author.clone(),
                    ci_status: CiStatus::Pending,
                    review_state: ReviewState::None,
                    is_merged: false,
                    is_closed: false,
                    needs_review: false,
                };
                self.prs.insert(*number, pr);
                self.open_prs.push(*number);
            }
            DomainEvent::PrUpdated {
                number,
                ci_status,
                review_state,
            } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.ci_status = ci_status.clone();
                    pr.review_state = review_state.clone();
                }
            }
            DomainEvent::PrMerged { number, .. } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.is_merged = true;
                    pr.is_closed = true;
                    pr.needs_review = false;
                }
                self.open_prs.retain(|n| n != number);
                self.needing_review.retain(|n| n != number);
            }
            DomainEvent::PrClosed { number } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.is_closed = true;
                    pr.needs_review = false;
                }
                self.open_prs.retain(|n| n != number);
                self.needing_review.retain(|n| n != number);
            }
            DomainEvent::PrReviewRequested { number } => {
                if let Some(pr) = self.prs.get_mut(number) {
                    pr.needs_review = true;
                }
                if !self.needing_review.contains(number) {
                    self.needing_review.push(*number);
                }
            }
            DomainEvent::PrLinkedToTask { number, task_id } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.pr_number = Some(*number);
                }
            }
            _ => {}
        }
    }

    pub fn pr_for_task(&self, id: &TaskId) -> Option<&PrState> {
        self.tasks.get(id)?.pr_number.and_then(|n| self.prs.get(&n))
    }

    pub fn task_for_pr(&self, pr: u64) -> Option<(&TaskId, &Task)> {
        self.tasks.iter().find(|(_, t)| t.pr_number == Some(pr))
    }

    pub fn pending_unblocked(&self) -> Vec<&TaskId> {
        self.pending_tasks
            .iter()
            .filter(|id| !self.blocked.contains_key(*id))
            .collect()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib daemon_v2::projections::work::tests 2>&1 | tail -15`
Expected: All 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/projections/work.rs src/daemon_v2/projections/work_tests.rs
git commit -m "feat(daemon-v2): implement WorkIndex projection with tasks and PRs"
```

---

### Task 5: ChannelIndex projection

**Files:**
- Modify: `src/daemon_v2/projections/channels.rs`
- Create: `src/daemon_v2/projections/channels_tests.rs`

- [ ] **Step 1: Write tests in src/daemon_v2/projections/channels_tests.rs**

```rust
use super::*;
use crate::daemon_v2::events::DomainEvent;

#[test]
fn message_creates_channel_if_missing() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "ghost-town".into(),
        content: "hello".into(),
        thread_id: None,
    });

    assert!(idx.channels.contains_key("main"));
    assert!(idx.channels.get("main").unwrap().last_message_at.is_some());
}

#[test]
fn thread_message_increments_thread_count() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "user".into(),
        content: "parent".into(),
        thread_id: None,
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m2".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply".into(),
        thread_id: Some("m1".into()),
    });

    assert_eq!(idx.channels.get("main").unwrap().thread_count, 1);
}

#[test]
fn multiple_replies_same_thread_no_double_count() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "user".into(),
        content: "parent".into(),
        thread_id: None,
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m2".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply 1".into(),
        thread_id: Some("m1".into()),
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m3".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply 2".into(),
        thread_id: Some("m1".into()),
    });

    // Still only 1 thread, not 2
    assert_eq!(idx.channels.get("main").unwrap().thread_count, 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon_v2::projections::channels::tests 2>&1 | tail -10`
Expected: Compilation errors — ChannelMeta and ChannelIndex fields don't exist.

- [ ] **Step 3: Implement ChannelIndex in src/daemon_v2/projections/channels.rs**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::daemon_v2::events::DomainEvent;

#[path = "channels_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelSettings {
    pub show_full_lead_output: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub archived: bool,
    pub settings: ChannelSettings,
    pub workflow: Option<String>,
    pub thread_count: usize,
    pub last_message_at: Option<DateTime<Utc>>,
    /// Track known thread parent IDs to avoid double-counting
    #[serde(default)]
    known_threads: HashSet<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ChannelIndex {
    pub channels: HashMap<String, ChannelMeta>,
    pub read_state: HashMap<String, DateTime<Utc>>,
}

impl ChannelIndex {
    pub fn apply(&mut self, event: &DomainEvent) {
        match event {
            DomainEvent::MessagePosted {
                channel, thread_id, ..
            } => {
                let meta = self.channels.entry(channel.clone()).or_insert_with(|| {
                    ChannelMeta {
                        name: channel.clone(),
                        archived: false,
                        settings: ChannelSettings::default(),
                        workflow: None,
                        thread_count: 0,
                        last_message_at: None,
                        known_threads: HashSet::new(),
                    }
                });
                meta.last_message_at = Some(Utc::now());

                if let Some(parent_id) = thread_id {
                    if meta.known_threads.insert(parent_id.clone()) {
                        meta.thread_count += 1;
                    }
                }
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib daemon_v2::projections::channels::tests 2>&1 | tail -15`
Expected: All 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/projections/channels.rs src/daemon_v2/projections/channels_tests.rs
git commit -m "feat(daemon-v2): implement ChannelIndex projection"
```

---

### Task 6: CooldownTracker

**Files:**
- Modify: `src/daemon_v2/projections/cooldowns.rs`
- Create: `src/daemon_v2/projections/cooldowns_tests.rs`

- [ ] **Step 1: Write tests in src/daemon_v2/projections/cooldowns_tests.rs**

```rust
use super::*;
use std::time::Duration;

#[test]
fn new_cooldown_is_active() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());

    assert!(tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"));
}

#[test]
fn unknown_cooldown_is_not_active() {
    let tracker = CooldownTracker::default();
    assert!(!tracker.is_active(CooldownCategory::OrphanSpawn, "agent-1"));
}

#[test]
fn different_key_not_active() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());

    assert!(!tracker.is_active(CooldownCategory::OrphanSpawn, "agent-2"));
}

#[test]
fn different_category_not_active() {
    let mut tracker = CooldownTracker::default();
    tracker.record(CooldownCategory::OrphanSpawn, "agent-1".into());

    assert!(!tracker.is_active(CooldownCategory::SpawnFailure, "agent-1"));
}

#[test]
fn category_durations_are_positive() {
    // Sanity check that all categories have non-zero durations
    let categories = [
        CooldownCategory::OrphanSpawn,
        CooldownCategory::AgentDispatch,
        CooldownCategory::SpawnFailure,
        CooldownCategory::MergeRebaseNudge,
        CooldownCategory::RebaseRegression,
        CooldownCategory::LeadWorktreeFreshness,
        CooldownCategory::TaskNudge,
        CooldownCategory::NoteStaleness,
    ];
    for cat in categories {
        assert!(cat.duration() > Duration::ZERO, "{cat:?} has zero duration");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon_v2::projections::cooldowns::tests 2>&1 | tail -10`
Expected: Compilation errors — CooldownCategory and methods don't exist.

- [ ] **Step 3: Implement CooldownTracker in src/daemon_v2/projections/cooldowns.rs**

```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[path = "cooldowns_tests.rs"]
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CooldownCategory {
    OrphanSpawn,
    AgentDispatch,
    SpawnFailure,
    MergeRebaseNudge,
    RebaseRegression,
    LeadWorktreeFreshness,
    TaskNudge,
    NoteStaleness,
}

impl CooldownCategory {
    pub fn duration(&self) -> Duration {
        match self {
            Self::OrphanSpawn => Duration::from_secs(60),
            Self::AgentDispatch => Duration::from_secs(30),
            Self::SpawnFailure => Duration::from_secs(120),
            Self::MergeRebaseNudge => Duration::from_secs(3600),
            Self::RebaseRegression => Duration::from_secs(3600),
            Self::LeadWorktreeFreshness => Duration::from_secs(300),
            Self::TaskNudge => Duration::from_secs(3600),
            Self::NoteStaleness => Duration::from_secs(3600),
        }
    }
}

#[derive(Debug, Default)]
pub struct CooldownTracker {
    entries: HashMap<(CooldownCategory, String), Instant>,
}

impl CooldownTracker {
    pub fn is_active(&self, category: CooldownCategory, key: &str) -> bool {
        self.entries
            .get(&(category, key.to_string()))
            .map(|t| t.elapsed() < category.duration())
            .unwrap_or(false)
    }

    pub fn record(&mut self, category: CooldownCategory, key: String) {
        self.entries.insert((category, key), Instant::now());
    }
}

// Cooldowns are ephemeral — not persisted across restarts.
// All cooldowns reset on daemon restart (safe, they're short-lived).
impl Serialize for CooldownTracker {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        s.serialize_struct("CooldownTracker", 0)?.end()
    }
}

impl<'de> Deserialize<'de> for CooldownTracker {
    fn deserialize<D: serde::Deserializer<'de>>(_d: D) -> Result<Self, D::Error> {
        // Skip any fields in the JSON — always return fresh tracker
        let _ = serde::de::IgnoredAny::deserialize(_d);
        Ok(Self::default())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib daemon_v2::projections::cooldowns::tests 2>&1 | tail -15`
Expected: All 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/daemon_v2/projections/cooldowns.rs src/daemon_v2/projections/cooldowns_tests.rs
git commit -m "feat(daemon-v2): implement unified CooldownTracker"
```

---

### Task 7: Wire projections together and test end-to-end event flow

**Files:**
- Modify: `src/daemon_v2/projections/mod.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Update src/daemon_v2/projections/mod.rs with full implementation**

Replace the stub `Projections` with the real version that dispatches to all sub-projections:

```rust
use serde::{Deserialize, Serialize};

use super::events::DomainEvent;

pub mod agents;
pub mod channels;
pub mod cooldowns;
pub mod work;

pub use agents::AgentIndex;
pub use channels::ChannelIndex;
pub use cooldowns::CooldownTracker;
pub use work::WorkIndex;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Projections {
    pub agents: AgentIndex,
    pub work: WorkIndex,
    pub channels: ChannelIndex,
    #[serde(skip)]
    pub cooldowns: CooldownTracker,
}

impl Projections {
    pub fn apply(&mut self, event: &DomainEvent) {
        self.agents.apply(event);
        self.work.apply(event);
        self.channels.apply(event);
    }

    pub fn apply_all(&mut self, events: &[DomainEvent]) {
        for event in events {
            self.apply(event);
        }
    }
}
```

- [ ] **Step 2: Verify all tests still pass**

Run: `cargo test --lib daemon_v2 2>&1 | tail -20`
Expected: All tests across all modules pass (16+ tests).

- [ ] **Step 3: Verify snapshot round-trip works end-to-end**

This is already covered by the `snapshot_and_recover` test in Task 2, but now with real projection data. Run:

Run: `cargo test --lib daemon_v2::events::store::tests::snapshot_and_recover 2>&1 | tail -10`
Expected: PASS — projections serialize to snapshot and deserialize back.

- [ ] **Step 4: Commit**

```bash
git add src/daemon_v2/
git commit -m "feat(daemon-v2): wire projections container with apply dispatch"
```

---

### Task 8: RPC server skeleton with status and agent.list

**Files:**
- Create: `src/daemon_v2/rpc/mod.rs`
- Create: `src/daemon_v2/rpc/handlers.rs`
- Create: `src/daemon_v2/rpc/rpc_tests.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Write tests in src/daemon_v2/rpc/rpc_tests.rs**

```rust
use super::*;
use crate::daemon_v2::events::*;
use crate::daemon_v2::Projections;
use serde_json::json;

fn projections_with_agents() -> Projections {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("task-1".into()),
    });
    proj.apply(&DomainEvent::AgentStarted {
        id: "a1".into(),
        pid: 1234,
    });
    proj.apply(&DomainEvent::AgentCreated {
        id: "a2".into(),
        name: "main-lead".into(),
        kind: AgentKind::Lead,
        agent_type: "midtown-channel-lead".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: None,
    });
    proj
}

#[test]
fn status_returns_agent_counts() {
    let proj = projections_with_agents();
    let result = handlers::handle_status(&proj);

    let result = result.unwrap();
    assert_eq!(result["agents"]["total"], 2);
    assert_eq!(result["agents"]["running"], 1);
}

#[test]
fn agent_list_returns_all_agents() {
    let proj = projections_with_agents();
    let result = handlers::handle_agent_list(&proj, None);

    let result = result.unwrap();
    let agents = result.as_array().unwrap();
    assert_eq!(agents.len(), 2);
}

#[test]
fn agent_list_filters_by_kind() {
    let proj = projections_with_agents();
    let filter = Some(AgentFilter {
        kind: Some(AgentKind::Worker),
        running_only: false,
    });
    let result = handlers::handle_agent_list(&proj, filter);

    let result = result.unwrap();
    let agents = result.as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "ghost-town");
}

#[test]
fn agent_list_filters_running_only() {
    let proj = projections_with_agents();
    let filter = Some(AgentFilter {
        kind: None,
        running_only: true,
    });
    let result = handlers::handle_agent_list(&proj, filter);

    let result = result.unwrap();
    let agents = result.as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "ghost-town");
}

#[test]
fn dispatch_routes_status() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "status",
        "id": 1
    });
    let response = dispatch_request(request, &proj);

    assert!(response["error"].is_null());
    assert!(response["result"]["agents"]["total"].is_number());
}

#[test]
fn dispatch_routes_agent_list() {
    let proj = projections_with_agents();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "agent.list",
        "id": 2
    });
    let response = dispatch_request(request, &proj);

    assert!(response["error"].is_null());
    assert!(response["result"].is_array());
}

#[test]
fn dispatch_unknown_method_returns_error() {
    let proj = Projections::default();
    let request = json!({
        "jsonrpc": "2.0",
        "method": "nonexistent",
        "id": 3
    });
    let response = dispatch_request(request, &proj);

    assert_eq!(response["error"]["code"], -32601);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib daemon_v2::rpc::tests 2>&1 | tail -10`
Expected: Compilation errors — rpc module doesn't exist yet.

- [ ] **Step 3: Create src/daemon_v2/rpc/handlers.rs**

```rust
use serde_json::{json, Value};

use crate::daemon_v2::events::AgentKind;
use crate::daemon_v2::Projections;

#[derive(Debug, Clone)]
pub struct AgentFilter {
    pub kind: Option<AgentKind>,
    pub running_only: bool,
}

impl AgentFilter {
    pub fn from_params(params: Option<&Value>) -> Option<Self> {
        let params = params?;
        let kind = params.get("kind").and_then(|v| v.as_str()).and_then(|s| {
            match s {
                "lead" => Some(AgentKind::Lead),
                "fork" => Some(AgentKind::Fork),
                "worker" => Some(AgentKind::Worker),
                _ => None,
            }
        });
        let running_only = params
            .get("running_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Some(Self { kind, running_only })
    }
}

pub fn handle_status(proj: &Projections) -> Result<Value, RpcError> {
    let total = proj.agents.by_id.len();
    let running = proj.agents.running.len();
    let pending_tasks = proj.work.pending_tasks.len();
    let in_progress_tasks = proj.work.in_progress_tasks.len();
    let open_prs = proj.work.open_prs.len();

    Ok(json!({
        "agents": {
            "total": total,
            "running": running,
        },
        "tasks": {
            "pending": pending_tasks,
            "in_progress": in_progress_tasks,
        },
        "prs": {
            "open": open_prs,
        },
    }))
}

pub fn handle_agent_list(
    proj: &Projections,
    filter: Option<AgentFilter>,
) -> Result<Value, RpcError> {
    let agents: Vec<Value> = proj
        .agents
        .by_id
        .values()
        .filter(|agent| {
            if let Some(ref f) = filter {
                if let Some(ref kind) = f.kind {
                    if &agent.kind != kind {
                        return false;
                    }
                }
                if f.running_only && !proj.agents.running.contains(&agent.id) {
                    return false;
                }
            }
            true
        })
        .map(|agent| {
            json!({
                "id": agent.id,
                "name": agent.name,
                "kind": agent.kind,
                "agent_type": agent.agent_type,
                "provider": agent.provider,
                "channel": agent.channel,
                "task_id": agent.task_id,
                "pid": agent.pid,
                "running": proj.agents.running.contains(&agent.id),
            })
        })
        .collect();

    Ok(json!(agents))
}

#[derive(Debug)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn method_not_found() -> Self {
        Self {
            code: -32601,
            message: "Method not found".into(),
        }
    }

    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
        }
    }

    pub fn to_json(&self, id: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "error": {
                "code": self.code,
                "message": self.message,
            },
            "id": id,
        })
    }
}
```

- [ ] **Step 4: Create src/daemon_v2/rpc/mod.rs**

```rust
pub mod handlers;

use handlers::{AgentFilter, RpcError};
use serde_json::{json, Value};

use crate::daemon_v2::Projections;

#[path = "rpc_tests.rs"]
#[cfg(test)]
mod tests;

pub fn dispatch_request(request: Value, proj: &Projections) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return RpcError {
                code: -32600,
                message: "Missing method".into(),
            }
            .to_json(&id);
        }
    };
    let params = request.get("params");

    let result = match method {
        "status" => handlers::handle_status(proj),
        "agent.list" => {
            let filter = AgentFilter::from_params(params);
            handlers::handle_agent_list(proj, filter)
        }
        _ => Err(RpcError::method_not_found()),
    };

    match result {
        Ok(value) => json!({
            "jsonrpc": "2.0",
            "result": value,
            "id": id,
        }),
        Err(err) => err.to_json(&id),
    }
}
```

- [ ] **Step 5: Add rpc module to daemon_v2/mod.rs**

Update `src/daemon_v2/mod.rs`:

```rust
pub mod events;
pub mod projections;
pub mod rpc;

pub use events::DomainEvent;
pub use projections::Projections;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib daemon_v2::rpc::tests 2>&1 | tail -20`
Expected: All 7 tests pass.

- [ ] **Step 7: Run all daemon_v2 tests**

Run: `cargo test --lib daemon_v2 2>&1 | tail -20`
Expected: All tests pass (23+ tests across all modules).

- [ ] **Step 8: Commit**

```bash
git add src/daemon_v2/
git commit -m "feat(daemon-v2): add RPC dispatch with status and agent.list handlers"
```

---

### Task 9: Integration test — full event flow through store and projections

**Files:**
- Create: `src/daemon_v2/integration_tests.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Write integration test in src/daemon_v2/integration_tests.rs**

This test exercises the full pipeline: events → store → projections → snapshot → recovery → RPC query.

```rust
use crate::daemon_v2::events::*;
use crate::daemon_v2::projections::Projections;
use crate::daemon_v2::rpc;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn full_lifecycle_through_store_and_projections() {
    let dir = TempDir::new().unwrap();
    let mut store = EventStore::new(dir.path().join("events"));
    let mut proj = Projections::default();

    // 1. Create a task
    let e1 = DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Fix auth bug".into(),
        channel: "main".into(),
        blocked_by: vec![],
    };
    store.append(&e1).unwrap();
    proj.apply(&e1);

    // 2. Create an agent
    let e2 = DomainEvent::AgentCreated {
        id: "a1".into(),
        name: "ghost-town".into(),
        kind: AgentKind::Worker,
        agent_type: "midtown-code-author".into(),
        provider: Provider::ClaudeCode,
        channel: Some("main".into()),
        task_id: Some("t1".into()),
    };
    store.append(&e2).unwrap();
    proj.apply(&e2);

    // 3. Start the agent
    let e3 = DomainEvent::AgentStarted { id: "a1".into(), pid: 5678 };
    store.append(&e3).unwrap();
    proj.apply(&e3);

    // Verify state via RPC
    let status = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
    );
    assert_eq!(status["result"]["agents"]["running"], 1);
    assert_eq!(status["result"]["tasks"]["pending"], 1);

    // 4. Assign task
    let e4 = DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "a1".into(),
    };
    store.append(&e4).unwrap();
    proj.apply(&e4);

    let status = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 2}),
        &proj,
    );
    assert_eq!(status["result"]["tasks"]["pending"], 0);
    assert_eq!(status["result"]["tasks"]["in_progress"], 1);

    // 5. Snapshot and recover
    store.save_snapshot(&proj).unwrap();

    // Add one more event after snapshot
    let e5 = DomainEvent::TaskCompleted { task_id: "t1".into() };
    store.append(&e5).unwrap();

    // Recover
    let (recovered_store, snapshot, replay_events) =
        EventStore::recover(dir.path().join("events")).unwrap();
    assert_eq!(recovered_store.sequence(), 5);

    let mut recovered_proj = snapshot.unwrap();
    recovered_proj.apply_all(&replay_events);

    // Verify recovered state matches
    let status = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 3}),
        &recovered_proj,
    );
    assert_eq!(status["result"]["tasks"]["in_progress"], 0);
    assert_eq!(status["result"]["agents"]["running"], 1);
}

#[test]
fn recover_from_empty_directory() {
    let dir = TempDir::new().unwrap();
    let (store, snapshot, events) =
        EventStore::recover(dir.path().join("events")).unwrap();

    assert_eq!(store.sequence(), 0);
    assert!(snapshot.is_none());
    assert!(events.is_empty());

    let proj = Projections::default();
    let status = rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
    );
    assert_eq!(status["result"]["agents"]["total"], 0);
}
```

- [ ] **Step 2: Add test module to daemon_v2/mod.rs**

Update `src/daemon_v2/mod.rs`:

```rust
pub mod events;
pub mod projections;
pub mod rpc;

pub use events::DomainEvent;
pub use projections::Projections;

#[path = "integration_tests.rs"]
#[cfg(test)]
mod integration_tests;
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test --lib daemon_v2::integration_tests 2>&1 | tail -15`
Expected: Both tests pass.

- [ ] **Step 4: Run full test suite to check for regressions**

Run: `cargo test 2>&1 | tail -20`
Expected: All existing tests plus new daemon_v2 tests pass.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`
Expected: No warnings.

- [ ] **Step 6: Commit**

```bash
git add src/daemon_v2/
git commit -m "feat(daemon-v2): add integration test for full event lifecycle"
```

---

## Summary

After completing all 9 tasks, the daemon_v2 module provides:

- **DomainEvent** enum with ~25 variants covering agents, tasks, PRs, chat, health, worktrees
- **EventStore** with append-only JSONL log, periodic snapshots, and crash-safe recovery
- **AgentIndex** projection with indexed lookups by id, name, task, channel + running set
- **WorkIndex** projection with combined tasks and PRs, blocking, and single-source PR→task link
- **ChannelIndex** projection with channel metadata and thread tracking
- **CooldownTracker** with unified category-based cooldowns (not persisted across restarts)
- **RPC dispatch** with `status` and `agent.list` endpoints
- **23+ unit tests** and **2 integration tests** covering the full pipeline

No v1 code is modified (except adding `pub mod daemon_v2;` to `lib.rs`). The two daemons coexist.
