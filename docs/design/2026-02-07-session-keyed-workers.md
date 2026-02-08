# Session-Keyed Workers: Design Proposal

**Author:** amsterdam
**Date:** 2026-02-07
**Task:** #920
**Status:** Design (not yet implemented)

## Problem Statement

Currently, coworker sessions are keyed by name (avenue names like "lexington", "park") throughout the system:
- `CoworkerManager` — `HashMap<String, Coworker>` keyed by name
- `SessionManager` — `HashMap<String, CoworkerSession>` keyed by name
- `coworker_records` — `HashMap<String, CoworkerRecord>` keyed by name
- Task ownership — `task.owner` is a coworker name string

This creates a hard limit: each named coworker can only run one task at a time. If "lexington" is working on task #42, they can't simultaneously work on task #43.

**Goal:** Enable parallel task execution by allowing multiple concurrent sessions per coworker identity (Variant A) or removing name-based identity entirely (Variant B).

**Prerequisite:** Task #912 (task-worktree wiring) must land first — each session needs its own isolated worktree.

## Current Architecture

### Name-Based Identity Throughout the Stack

```
┌─────────────────────────────────────────────────────────────┐
│                     Coworker Names                           │
│          (lexington, park, madison, broadway, ...)           │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
        ▼                     ▼                     ▼
┌──────────────┐   ┌─────────────────┐   ┌─────────────────┐
│CoworkerManager│   │ SessionManager  │   │coworker_records│
│  HashMap<    │   │   HashMap<      │   │   HashMap<     │
│   name,      │   │    name,        │   │    name,       │
│   Coworker>  │   │    Session>     │   │    Record>     │
└──────────────┘   └─────────────────┘   └─────────────────┘
        │                     │                     │
        └─────────────────────┼─────────────────────┘
                              │
                              ▼
                      ┌──────────────┐
                      │  Task Owner  │
                      │  (String)    │
                      └──────────────┘
```

### Key Constraints

1. **Name pool limit** — 10 primary avenues + 6 overflow streets = 16 max concurrent coworkers
2. **One task per name** — Each name can only own/work on one task at a time
3. **Name reuse requires cleanup** — Can't reuse "lexington" until previous session fully shuts down
4. **Deduplication by name** — `dedup_spawn_effects()` prevents spawning duplicate names

### Where Names Matter

**Critical (must change for session-keying):**
- `CoworkerManager::coworkers` — HashMap key
- `SessionManager::sessions` — HashMap key
- `coworker_records` — HashMap key
- `task.owner` — Ownership tracking
- Spawn deduplication — Currently by name

**Display only (can keep names):**
- Channel messages — Sender identity
- Tmux window names — Human-readable labels
- Web UI — Coworker cards
- Nudge targeting — Need to route to correct session

## Variant A: Multi-Session Per Name

### Core Idea

Preserve coworker names as stable identities, but allow each name to run multiple concurrent sessions. Key sessions by `(name, session_id)` compound keys instead of just `name`.

### Architecture Changes

```
┌─────────────────────────────────────────────────────────────┐
│              Coworker Names (display identity)               │
│          (lexington, park, madison, broadway, ...)           │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
        ▼                     ▼                     ▼
┌──────────────┐   ┌─────────────────┐   ┌─────────────────┐
│CoworkerManager│   │ SessionManager  │   │coworker_records│
│  HashMap<    │   │   HashMap<      │   │   HashMap<     │
│   name,      │   │    SessionKey,  │   │    SessionKey, │
│   Vec<Sess>> │   │    Session>     │   │    Record>     │
└──────────────┘   └─────────────────┘   └─────────────────┘
        │                     │                     │
        └─────────────────────┼─────────────────────┘
                              │
                              ▼
                      ┌──────────────┐
                      │  Task Owner  │
                      │ (SessionKey) │
                      └──────────────┘

SessionKey = (name: String, session_id: String)
```

#### SessionKey Structure

```rust
/// Compound key for coworker sessions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    /// Coworker name (lexington, park, etc.) — display identity
    pub name: String,
    /// Claude Code session UUID — unique per concurrent session
    pub session_id: String,
}

impl SessionKey {
    /// Display format for channel messages: "lexington/task-42" or just "lexington"
    pub fn display_name(&self) -> String {
        // Simple: just use the name for channel readability
        self.name.clone()
    }

    /// Full identifier for internal keying: "lexington:abc-123-def"
    pub fn full_id(&self) -> String {
        format!("{}:{}", self.name, self.session_id)
    }
}
```

#### Key Changes

**1. SessionManager (src/daemon/sessions.rs)**

```rust
pub struct SessionManager {
    // OLD: sessions: RwLock<HashMap<String, CoworkerSession>>,
    sessions: RwLock<HashMap<SessionKey, CoworkerSession>>,
}

impl SessionManager {
    pub async fn spawn(
        &self,
        name: &str,
        config: &HeadlessConfig,
        initial_prompt: Option<&str>,
    ) -> Result<SessionKey, crate::Error> {
        // Spawn the session
        let mut session = HeadlessSession::spawn(config)?;

        // Wait for init event to get session_id (or generate if headless doesn't provide)
        let session_id = wait_for_session_id(&mut session).await?;

        let key = SessionKey {
            name: name.to_string(),
            session_id,
        };

        // Check for duplicate session_id (not duplicate name — multiple names OK)
        if self.sessions.read().await.contains_key(&key) {
            return Err(...);
        }

        // Insert under compound key
        self.sessions.write().await.insert(
            key.clone(),
            CoworkerSession::new(name.to_string(), session),
        );

        Ok(key)
    }

    pub async fn send_message(&self, key: &SessionKey, message: &str) -> Result<(), crate::Error> {
        // Route to specific session via compound key
        let mut sessions = self.sessions.write().await;
        let session = sessions.get_mut(key).ok_or(...)?;
        session.session.as_mut()?.send_message(message).await
    }
}
```

**2. CoworkerManager (src/coworker.rs)**

```rust
pub struct CoworkerManager {
    // OLD: coworkers: Arc<RwLock<HashMap<String, Coworker>>>,
    // NEW: Track multiple sessions per name
    coworkers: Arc<RwLock<HashMap<SessionKey, Coworker>>>,
    // ...existing fields
}

impl CoworkerManager {
    pub async fn register_coworker(
        &self,
        name: &str,
        session_id: &str,
        working_dir: String,
    ) -> Result<SessionKey, WorktreeError> {
        let key = SessionKey {
            name: name.to_string(),
            session_id: session_id.to_string(),
        };

        let coworker = Coworker {
            name: name.to_string(),
            session_id: Some(session_id.to_string()),
            working_dir,
            status: CoworkerStatus::Starting,
            started_at: Utc::now(),
            current_task: None,
            isolated_tasks: false,
        };

        self.coworkers.write().await.insert(key.clone(), coworker);
        Ok(key)
    }

    /// Get all sessions for a given coworker name.
    pub fn sessions_for_name(&self, name: &str) -> Vec<SessionKey> {
        self.coworkers.read().unwrap()
            .keys()
            .filter(|k| k.name == name)
            .cloned()
            .collect()
    }
}
```

**3. Task Ownership (src/tasks.rs)**

```rust
// OLD: pub owner: Option<String>,  // coworker name
// NEW: pub owner: Option<SessionKey>,  // compound key

// Serialization: Store as "lexington:abc-123-def" string in task JSON,
// parse back to SessionKey on load.

impl Task {
    pub fn owner_display_name(&self) -> Option<String> {
        self.owner.as_ref().map(|k| k.display_name())
    }
}

// Update functions:
pub fn update_task_owner(task_id: &str, owner: &SessionKey) -> Result<(), String> {
    // Serialize SessionKey to full_id() for JSON storage
}

pub fn get_in_progress_tasks_with_owners() -> Vec<(String, SessionKey)> {
    // Parse owner strings back to SessionKey
}
```

**4. coworker_records (src/daemon/mod.rs, src/rules.rs)**

```rust
// In DaemonState:
// OLD: coworker_records: tokio::sync::RwLock<HashMap<String, CoworkerRecord>>,
// NEW:
coworker_records: tokio::sync::RwLock<HashMap<SessionKey, CoworkerRecord>>,

// In RPC handlers (src/daemon/rpc.rs):
pub async fn handle_state_report(
    state: &DaemonState,
    name: &str,
    session_id: &str,  // NEW: Must be provided by coworker via RPC
    phase: WorkflowPhase,
    task_id: Option<u32>,
) -> Result<Value, crate::Error> {
    let key = SessionKey {
        name: name.to_string(),
        session_id: session_id.to_string(),
    };

    let mut records = state.coworker_records.write().await;
    crate::rules::set_workflow(&mut records, &key, phase, task_id);
    // ...
}
```

**5. Spawn Deduplication (src/daemon/effects.rs)**

```rust
// OLD: Dedupe by name — prevent duplicate "lexington" spawns
pub fn dedup_spawn_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut seen = HashSet::new();
    effects.into_iter().filter(|e| {
        match e {
            Effect::AssignAndSpawn { config, .. } => {
                seen.insert(config.name.clone())
            }
            // ...
        }
    }).collect()
}

// NEW: Dedupe by (name, task_id) — prevent duplicate work on same task,
// but allow same name to work on different tasks concurrently
pub fn dedup_spawn_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut seen = HashSet::new();
    effects.into_iter().filter(|e| {
        match e {
            Effect::AssignAndSpawn { config, task, .. } => {
                // Key by (name, task_id) instead of just name
                let key = (config.name.clone(), task.id.clone());
                seen.insert(key)
            }
            // ...
        }
    }).collect()
}
```

**6. Channel Messages**

Channel sender remains the coworker name (not the full SessionKey) for readability. The channel log already uses names as sender strings, so no changes needed:

```rust
// Existing channel API already uses &str for sender — just use SessionKey.name
channel.post("lexington", "completed task 42");
```

**7. Nudge Routing**

Nudges need to route to a specific session, not just a name. The daemon must track which SessionKey is working on which task to route nudges correctly:

```rust
// In Effect::NudgeCoworker:
Effect::NudgeCoworker {
    key: SessionKey,  // NEW: Route to specific session
    message: String,
}

// Daemon must look up SessionKey from task ownership before issuing nudge
```

### Pros

✅ **Preserves channel readability** — Messages still show "lexington", not "lexington:abc-123"
✅ **Backward compatible display** — Web UI, tmux tabs can still show simple names
✅ **Clear ownership model** — Task owner is a concrete SessionKey, not ambiguous
✅ **Incremental migration** — Can land SessionKey types first, populate session_id field gradually
✅ **Familiar mental model** — "lexington is working on multiple tasks" is intuitive

### Cons

❌ **Complexity increase** — Every name-keyed HashMap becomes SessionKey-keyed
❌ **Session ID plumbing** — RPC calls must provide session_id (currently optional)
❌ **Routing ambiguity** — Need to track which session owns which task for nudges
❌ **Partial identity** — Name is display-only, SessionKey is the real identity

### Migration Path

1. **Add SessionKey type** — Newtype wrapper, implement serialization
2. **Extend RPC protocol** — Add `session_id` parameter to state reports (optional for backward compat)
3. **Update SessionManager** — Key by SessionKey, return SessionKey from spawn
4. **Update CoworkerManager** — Key by SessionKey
5. **Update coworker_records** — Key by SessionKey
6. **Update task ownership** — Parse/serialize SessionKey in Task.owner
7. **Update spawn dedup** — Key by (name, task_id) instead of name
8. **Update nudge routing** — Look up SessionKey from task assignments
9. **Tests** — Multi-session scenarios, concurrent tasks per name

---

## Variant B: Fully Anonymous Workers

### Core Idea

Drop coworker names entirely as primary keys. Key everything by task ID or session UUID. Names become optional display labels with no semantic meaning.

### Architecture Changes

```
                     ┌──────────────┐
                     │   Task ID    │
                     │  (Primary)   │
                     └──────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        │                   │                   │
        ▼                   ▼                   ▼
┌──────────────┐   ┌─────────────────┐   ┌─────────────────┐
│SessionManager│   │WorktreeRegistry │   │coworker_records│
│  HashMap<    │   │   HashMap<      │   │   HashMap<     │
│   task_id,   │   │    task_id,     │   │    task_id,    │
│   Session>   │   │    Worktree>    │   │    Record>     │
└──────────────┘   └─────────────────┘   └─────────────────┘
        │                   │                   │
        └───────────────────┼───────────────────┘
                            │
                            ▼
                    ┌──────────────┐
                    │ Display Name │
                    │  (Optional)  │
                    └──────────────┘
```

#### Key Changes

**1. Session Manager**

```rust
pub struct SessionManager {
    // OLD: sessions: RwLock<HashMap<String, CoworkerSession>>,
    // NEW: Key by task ID (or session UUID for non-task work)
    sessions: RwLock<HashMap<String, CoworkerSession>>,
}

pub struct CoworkerSession {
    session: Option<HeadlessSession>,
    /// Task ID this session is working on (primary key)
    pub task_id: String,
    /// Optional display name for UI/channel (lexington, park, etc.)
    pub display_name: Option<String>,
    pub status: SessionStatus,
    pub started_at: DateTime<Utc>,
    pub session_id: Option<String>,
    // ...
}

impl SessionManager {
    pub async fn spawn_for_task(
        &self,
        task_id: &str,
        display_name: Option<&str>,
        config: &HeadlessConfig,
        initial_prompt: Option<&str>,
    ) -> Result<(), crate::Error> {
        // Dedupe by task_id, not name
        if self.sessions.read().await.contains_key(task_id) {
            return Err(...);  // Already working on this task
        }

        let session = HeadlessSession::spawn(config)?;
        self.sessions.write().await.insert(
            task_id.to_string(),
            CoworkerSession {
                task_id: task_id.to_string(),
                display_name: display_name.map(String::from),
                session: Some(session),
                // ...
            },
        );
        Ok(())
    }
}
```

**2. Task Ownership**

```rust
// Task owner is just the task ID itself — circular but simple
pub struct Task {
    pub id: String,
    // No owner field needed — a session exists for this task or it doesn't
    // ...
}

// Ownership query becomes: is there a session for this task_id?
pub fn task_is_assigned(task_id: &str, sessions: &SessionManager) -> bool {
    sessions.has_session_for_task(task_id)
}
```

**3. Display Names**

Display names are allocated from the avenue pool for UI/channel labels, but have no semantic meaning:

```rust
pub struct DisplayNamePool {
    available: Vec<String>,  // lexington, park, madison, ...
}

impl DisplayNamePool {
    pub fn allocate(&mut self) -> Option<String> {
        self.available.pop()
    }

    pub fn release(&mut self, name: String) {
        self.available.push(name);
    }
}

// On spawn:
let display_name = name_pool.allocate().unwrap_or_else(|| {
    format!("worker-{}", task_id)  // Fallback if pool exhausted
});

// On shutdown:
if let Some(name) = session.display_name {
    name_pool.release(name);
}
```

**4. Channel Messages**

Channel sender is the display name (if available) or task ID:

```rust
let sender = session.display_name.as_deref()
    .unwrap_or(&session.task_id);
channel.post(sender, "completed task");
```

**5. Nudge Routing**

Nudges route directly by task ID:

```rust
Effect::NudgeTask {
    task_id: String,
    message: String,
}

// Execute:
session_manager.send_message_to_task(&task_id, &message).await?;
```

**6. Spawn Deduplication**

```rust
// Dedupe by task_id — already implicit in HashMap::insert()
// No explicit deduplication needed if sessions keyed by task_id
```

### Pros

✅ **Unlimited concurrency** — No name pool limit (16 max → unbounded)
✅ **Simpler mental model** — One session per task, period
✅ **Direct routing** — Nudge task #42 → session for task #42
✅ **No name reuse issues** — Names are ephemeral labels
✅ **Clean separation** — Identity (task) vs. display (name)

### Cons

❌ **Breaking change** — Incompatible with current name-keyed architecture
❌ **Channel readability risk** — Messages from "worker-739" are less human-friendly
❌ **Name pool may still exhaust** — If we want pretty names, still limited to 16 concurrent
❌ **Task-less sessions tricky** — How to handle review work that's not task-based?
❌ **More radical change** — Requires rethinking ownership model entirely

### Migration Path

1. **Introduce task-keyed SessionManager** — New implementation alongside old
2. **Update spawn path** — Route task spawns through new manager
3. **Update nudge routing** — Use task_id instead of name
4. **Replace CoworkerManager** — Merge into SessionManager (no separate concept)
5. **Update channel protocol** — Accept task_id or display_name as sender
6. **Remove name-based deduplication** — Task-id dedup is implicit
7. **Migrate coworker_records** — Key by task_id
8. **Tests** — Task-keyed spawning, concurrent tasks, display name allocation

---

## Comparison Matrix

| Aspect | Current | Variant A (Multi-Session) | Variant B (Anonymous) |
|--------|---------|---------------------------|----------------------|
| **Max concurrency** | 16 (name pool) | 16 × N (sessions per name) | Unbounded |
| **Primary key** | Name (String) | SessionKey (name + UUID) | Task ID (String) |
| **Task owner** | Name (String) | SessionKey | Task ID (implicit) |
| **Channel sender** | Name | Name (from SessionKey) | Display name or task_id |
| **Dedup strategy** | By name | By (name, task) | By task_id |
| **Nudge routing** | Name | SessionKey | Task ID |
| **Display labels** | Name (semantic) | Name (semantic) | Name (cosmetic only) |
| **Migration effort** | N/A | Moderate (extend keys) | High (rearchitect) |
| **Backward compat** | N/A | Good (names still valid) | Breaking (names optional) |
| **Conceptual clarity** | Simple | Medium | Clean separation |

---

## Recommendation

**Start with Variant A (Multi-Session Per Name)** for the following reasons:

1. **Incremental migration** — Can be done in stages without breaking existing behavior
2. **Preserves UX** — Channel messages still say "lexington", not "worker-739"
3. **Sufficient scaling** — 16 names × 10 concurrent tasks = 160 max sessions (plenty)
4. **Backward compatible** — Existing single-session-per-name works as SessionKey with one session
5. **Testable** — Can validate multi-session behavior alongside current single-session

**Consider Variant B later** if we hit these conditions:
- Need >160 concurrent sessions (unlikely)
- Want to fully decouple identity from display
- Ready for a major version bump / breaking change

---

## Implementation Plan (Variant A)

### Phase 1: Type Infrastructure (No behavior change)

1. Add `SessionKey` type (src/session_key.rs)
2. Add serialization for SessionKey (to/from "name:uuid" string)
3. Add helper methods (display_name(), full_id(), parse())

### Phase 2: SessionManager Upgrade

4. Change SessionManager to key by SessionKey
5. Update spawn() to return SessionKey
6. Update send_message() to accept SessionKey
7. Update drain_events() to emit SessionKey in events

### Phase 3: CoworkerManager Upgrade

8. Change CoworkerManager to key by SessionKey
9. Update register_coworker() to return SessionKey
10. Add sessions_for_name() helper

### Phase 4: State Tracking

11. Change coworker_records to key by SessionKey
12. Update RPC handlers to parse session_id from coworker state reports
13. Update recover_coworker_records() to handle SessionKey

### Phase 5: Task Ownership

14. Change Task.owner to Option<SessionKey>
15. Update task JSON serialization (owner = "name:uuid" string)
16. Update all task ownership queries (get_in_progress_tasks_with_owners, etc.)

### Phase 6: Effects & Nudges

17. Update spawn dedup to key by (name, task_id)
18. Update nudge effects to target SessionKey
19. Update nudge routing to look up SessionKey from task assignments

### Phase 7: RPC Protocol

20. Add session_id parameter to midtown CLI state command
21. Update coworker system prompt to report session_id via RPC
22. Handle backward compat (session_id optional, default to name)

### Phase 8: Tests

23. Unit tests for SessionKey serialization
24. Unit tests for multi-session spawn (same name, different tasks)
25. Unit tests for spawn dedup (same task = blocked, different task = allowed)
26. E2E test: Spawn 2 sessions for "lexington", different tasks
27. E2E test: Nudge routes to correct session

### Phase 9: Cleanup

28. Remove dead code paths for single-session assumptions
29. Update documentation (CLAUDE.md, README.md)
30. Add migration guide for custom deployments

---

## Open Questions

### Variant A

1. **Session ID source** — Does headless Claude Code reliably provide session UUID in init event? If not, generate our own?
2. **RPC backward compat** — How long do we support session_id being optional in state reports?
3. **Nudge targeting** — If a nudge mentions "@lexington" but lexington has 3 sessions, which one gets it? (Answer: Route to the task context)
4. **Display in web UI** — Show "lexington (3 active)" or list each session separately?
5. **Name reuse timing** — Can we reuse "lexington" as a display name for a new session before the old one fully shuts down?

### Variant B

1. **Review workflows** — Reviews aren't task-based in current model. Key by PR number instead of task ID?
2. **Display name exhaustion** — If we still use pretty names and run >16 concurrent, fall back to generic names or block spawning?
3. **Channel mentions** — How does "@lexington" work if lexington is just a cosmetic label?
4. **Historical logs** — Old channel messages reference names. How to trace back to task if name was reused?

---

## Dependencies

**Blockers:**
- ✅ Task #912 (WorktreeRegistry wiring) — DONE (merged PR #752)

**Follow-ups:**
- Display name pool management (Variant B)
- Multi-session UI in web app
- Session health monitoring updates
- Revised stuck detection (per-session, not per-name)

---

## References

- Task #912: Wire WorktreeRegistry into spawn path
- Task #920: Design: Session-keyed workers (this task)
- src/worktree_registry.rs — Task/PR to worktree mapping
- src/daemon/sessions.rs — Current SessionManager (name-keyed)
- src/coworker.rs — Current CoworkerManager (name-keyed)
- src/tasks.rs — Task ownership (currently name-keyed)
- src/rules.rs — CoworkerRecord (currently name-keyed)
