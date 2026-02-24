<div align="center"><img src="docs/images/logo.png" width="100" alt="Midtown logo"></div>

# Midtown

Midtown provides a familiar, unified interface for collaborating with a team of coding agents, coordinated through IRC-style channels in the terminal, the web, and a mobile PWA.

## How Midtown Works

Each channel is led by a specialized agent (a “channel lead”) with long-running context in that domain, while a project lead maintains a broad view of the overall effort. You can collaborate at either level: work with the project lead on planning and direction, or engage directly with any channel lead to brainstorm within a specific area. It works much like an organization: broad communication in #general, with focused discussions in specialized channels (eg. #webui, #api, #proj-tanstack-adoption, etc).

After a brainstorm, the channel lead turns the outcome into an execution plan (using skills) and creates tasks using the Claude Code native Tasks system. Those tasks include dependencies, so Midtown can distinguish what can run in parallel from what must happen in sequence.

Coworkers are the agents created to pick up and execute those tasks. Coworkers operate headlessly, but collaborate in the channels with you & the leads. You can also pair program directly with a Coworker and "attach" to their session. This becomes like working directly in the coding agent (currently Claude Code & Codex are supported).

## Quick Start

### 0. Prereqs

1. Install the [GitHub CLI](https://cli.github.com/).
2. Install [Rust & Cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html).

### 1. Install

From [crates.io](https://crates.io/crates/midtown):

```bash
cargo install midtown
```

Or from source:

```bash
cargo install --path .
```

### 2. Start midtown

From your project directory:

```bash
midtown start
```

For multi-repo projects, specify a project name and additional repos:

```bash
midtown start --project myapp --add-repo /path/to/frontend --add-repo /path/to/shared-lib
```

### 3. Start the TUI

If you're in a repo that's part of a project:

```bash
midtown view
```

You're now in the Zellij session with the Project Lead's Claude Code instance.

To attach to a named project from any directory:

```bash
midtown view myapp
```

### 4. Work with the Project Lead as you typically would work with Claude Code

The Project Lead is just a Claude Code session, but it's been booted with a [special system prompt](agents/lead.md). The system prompt instructs the Project Lead how to execute in the midtown environment—mostly to not take on work itself (unless it's trivial) and to instead make Claude Code tasks.

## CLI Overview

| Command | Description |
|---------|-------------|
| `midtown start` | Start the daemon and Zellij session |
| `midtown stop` | Stop everything |
| `midtown restart [--force]` | Restart the daemon (`--force` skips reviewer drain wait) |
| `midtown status` | Show system status |
| `midtown view` | Launch chat UI (use `--attach` to open Project Lead in a split) |
| `midtown channel post <msg>` | Post to the team channel |
| `midtown channel read` | Read recent messages |
| `midtown channel create <name>` | Create a new topic channel |
| `midtown channel unarchive <name>` | Restore an archived channel |
| `midtown channel rename <old> <new>` | Rename a channel |
| `midtown channel archive <name>` | Archive a channel |
| `midtown session view <target>` | View a session's recent output with rich ANSI rendering |
| `midtown session view <target> --watch` | Tail a headless session's output as new events arrive |
| `midtown session attach name/<n>` | Attach to a headless session |
| `midtown session detach <name>` | Resume headless execution |
| `midtown session clear <lookup>` | Stop and restart a session fresh (preserves original task prompt) |
| `midtown session fork <thread-id>` | Fork the calling session into a thread-bound session |
| `midtown task create <subject> [...]` | Create a task (see [CLI reference](docs/cli.md) for all options) |
| `midtown task list` | List tasks |
| `midtown task view <id>` | View task details |
| `midtown pr list` | List pull requests |
| `midtown config get/set/list` | Manage [configuration](docs/configuration.md) |
| `midtown auth` | Manage [auth profiles](docs/authentication.md) (multiple accounts supported) |

See the [full CLI reference](docs/cli.md) for all flags and options.

> **Note:** `midtown view` also accepts `midtown chat` and `midtown attach` as aliases.

## Documentation

| Topic | Description |
|-------|-------------|
| [CLI Reference](docs/cli.md) | Full command reference with all flags and agent-internal commands |
| [Configuration](docs/configuration.md) | Global and per-project config files, environment variables, custom prompts |
| [Authentication](docs/authentication.md) | Multi-account auth profiles, switching, storage |
| [Architecture](docs/architecture.md) | Daemon, coworkers, channel sync, GitHub integration, web UI |
| [Docker](docs/docker.md) | Docker images, running in containers |

### Hidden/Advanced Commands

These commands are not shown in `midtown --help` but remain fully functional. They are used internally by the daemon, coworker hooks, and agent prompts.

#### One-shot Execution

Run Claude Code sessions non-interactively with JSON streaming output:

```bash
midtown oneshot "Summarize this codebase" --model sonnet
midtown oneshot "Generate a report" --json-schema '{"type": "object", ...}'
midtown oneshot "Fix the bug" --allow-tools --max-budget-usd 0.50
```

| Flag | Description |
|------|-------------|
| `--model <name>` | Model to use (default: `sonnet`) |
| `--system-prompt <text>` | System prompt for the session |
| `--json-schema <json>` | JSON schema for structured output |
| `--max-budget-usd <float>` | Maximum budget in USD |
| `--allow-tools` | Allow tool use (default: no tools) |

#### Lead Commands

| Command | Description |
|---------|-------------|
| `midtown lead remind all-work-merged <message>` | Set a reminder for when all work is merged |
| `midtown lead remind list` | List active reminders |
| `midtown lead remind cancel <id>` | Cancel a reminder |

#### Webserver

The multi-project webserver serves the web UI and proxies to per-project daemons.

| Command | Description |
|---------|-------------|
| `midtown webserver run [--port 47022] [--foreground]` | Start the webserver |
| `midtown webserver stop` | Stop the webserver |
| `midtown webserver restart` | Restart the webserver |

#### Logs

| Command | Description |
|---------|-------------|
| `midtown log` | View daemon logs (follows by default) |
| `midtown log --hooks` | View hooks log |
| `midtown log --path` | Print log file path |

#### E2E Testing

| Command | Description |
|---------|-------------|
| `midtown e2e auth` | One-time auth setup for container testing |
| `midtown e2e run coordination` | Run coordination E2E tests (fast, no auth) |
| `midtown e2e run full` | Run full E2E tests (requires auth) |
| `midtown e2e capture [--label <name>]` | Capture daemon state snapshot for test fixtures |

### Test Isolation

Unit and integration tests should not rely on your real `~/.midtown` or `~/.claude`.
Run tests with an isolated `HOME`:

```bash
./scripts/with-isolated-home.sh cargo test
```

### Fake Provider CLIs (Deterministic E2E)

This workspace includes standalone fake provider CLIs for deterministic protocol testing:

- `fake-claude-cli` (simulates Claude stream-json + plugin commands)
- `fake-codex-cli` (simulates Codex `app-server` JSON-RPC)

Build them:

```bash
cargo build -p fake-claude-cli -p fake-codex-cli
```

Both support environment-variable controlled modes (echo, error, tool events, hang/no-response)
so tests can reproduce timeout/restart/race scenarios reliably. See each crate README:

- `crates/fake-claude-cli/README.md`
- `crates/fake-codex-cli/README.md`

## Configuration

Midtown uses two levels of config files:

1. **Global config** at `~/.midtown/config.toml` — applies to all projects
2. **Project config** at `~/.midtown/projects/<project>/config.toml` — overrides per project

Project settings take precedence over global defaults. All fields are optional.

### Global `config.toml`

```toml
# ~/.midtown/config.toml

[default]
bin_command = "midtown"         # CLI command to invoke midtown
chat_layout = "auto"            # "auto", "split", or "window"
chat_min_width = 160            # Min terminal width for split layout (auto mode)
zellij_swap_layout = false      # Lead left + chat right when true
zellij_chat_pane_size = 35      # Chat pane width percentage (10-90)
max_coworkers = 10              # Maximum concurrent coworkers

[daemon]
webhook_port = 47022                  # Web UI & webhook port (0 to disable)
webhook_secret = "your-secret"        # GitHub webhook signature secret
webhook_restart_interval_secs = 300   # Webhook forwarder restart interval
pr_poll_interval_secs = 30            # PR polling interval
chat_monitor_enabled = true           # Enable @mention routing

[execution]
lead_provider = "claude"              # Default for all leads ("claude", "codex", or "zai")
project_lead_provider = "zai"         # Optional override for project/main lead only
coworker_provider = "codex"           # Default provider for dev coworkers
reviewer_provider = "claude"          # Independent default for reviewers
review_mode = "both"                  # "local", "github_app", or "both"
channel_lead_provider = "codex"       # Optional channel-lead override (falls back to lead_provider)
specialized_provider = "claude"       # Default for specialized workers
headless_execute_provider = "claude"  # Optional override (fallback: specialized_provider)
```

### Project `config.toml`

Project configs support all global settings plus project metadata:

```toml
# ~/.midtown/projects/myapp/config.toml

[project]
name = "myapp"
repos = ["/path/to/backend", "/path/to/frontend"]
primary_repo = "/path/to/backend"

[default]
bin_command = "cargo run --release --"
max_coworkers = 4
zellij_swap_layout = true       # Project-specific override
zellij_chat_pane_size = 40      # Wider chat for this project

[daemon]
webhook_port = 47023              # Auto-assigned if not set

[execution]
lead_provider = "codex"           # Shared default for project + channel leads in this project
project_lead_provider = "zai"     # Optional override for project lead only
reviewer_provider = "claude"      # Keep reviewers independent
review_mode = "local"             # Force local reviewer coworkers for this project
```

The `[project]` section defines:

- `name` - Project name used for Zellij sessions, paths, etc.
- `repos` - List of repository paths belonging to the project
- `primary_repo` - The main repo used for the daemon socket and channel

For single-repo projects, only `name` is needed; `repos` and `primary_repo` are inferred from the working directory. This config is auto-created on first `midtown start`.

Execution provider resolution is role-based:

- Project lead: `execution.project_lead_provider` -> `execution.lead_provider` -> `claude`
- Dev coworkers: `execution.coworker_provider` (default: `claude`)
- Reviewers: `execution.reviewer_provider` (default: `claude`)
- Channel leads: `execution.channel_lead_provider` -> `execution.lead_provider` -> `claude`
- Specialized workers:
 - `oneshot.execute`: `execution.headless_execute_provider` -> `execution.specialized_provider` -> `claude`

Review execution mode controls how PR reviews are sourced:

- `execution.review_mode = "local"`: daemon spawns local reviewer coworkers
- `execution.review_mode = "github_app"`: daemon does not spawn local reviewers; waits for formal GitHub App reviews
- `execution.review_mode = "both"`: local reviewers are enabled and GitHub App/formal reviews also count

Model aliases are auto-normalized per provider at launch:

- Generic sizes:
  - Claude: `small` -> `haiku`, `medium` -> `sonnet`, `large` -> `opus`
  - z.ai: `small` -> `haiku`, `medium` -> `sonnet`, `large` -> `opus`
  - Codex: `small` -> `gpt-5.1-codex-mini`, `medium`/`large` -> `gpt-5.3-codex`
- z.ai model aliases are hard-mapped at launch via env vars:
  - `ANTHROPIC_DEFAULT_HAIKU_MODEL=GLM-4.5-Air`
  - `ANTHROPIC_DEFAULT_SONNET_MODEL=GLM-4.7`
  - `ANTHROPIC_DEFAULT_OPUS_MODEL=GLM-5`
- Cross-provider safety:
  - `haiku`/`sonnet`/`opus` are normalized to Codex defaults when provider is Codex.
  - `gpt-5-codex` is normalized to Claude/z.ai role defaults when provider is Claude/z.ai.

### Managing Config via CLI

You can read and write config settings without editing TOML files directly:

```bash
# List all settings (project config, inferred from cwd)
midtown config list

# List global settings
midtown config list --global

# Get a single setting
midtown config get default.max_coworkers
midtown config get daemon.webhook_port --global

# Set a setting (persisted to config.toml, comments preserved)
midtown config set default.max_coworkers 6
midtown config set daemon.webhook_port 47099 --global
```

Without `--global`, commands operate on the project config inferred from the current directory. With `--global`, they operate on `~/.midtown/config.toml`.

### Environment Variable Overrides

Daemon settings can be overridden with environment variables:

| Variable | Overrides |
|----------|-----------|
| `MIDTOWN_WEBHOOK_PORT` | `webhook_port` |
| `MIDTOWN_WEBHOOK_SECRET` | `webhook_secret` |
| `MIDTOWN_WEBHOOK_RESTART_INTERVAL` | `webhook_restart_interval_secs` |
| `MIDTOWN_PR_POLL_INTERVAL` | `pr_poll_interval_secs` |
| `MIDTOWN_CHAT_MONITOR` | `chat_monitor_enabled` (set to `0` to disable) |
| `MIDTOWN_MAX_COWORKERS` | `max_coworkers` |

## Docker

Midtown is available as a Docker image for containerized deployments. The image includes all runtime dependencies (Zellij, git, gh CLI, Claude CLI).

### Pull from Docker Hub

```bash
docker pull btucker/midtown:latest
# Or a specific version:
docker pull btucker/midtown:0.4.5
```

### Running with Docker

Mount your repository and optionally your midtown config:

```bash
# Basic usage
docker run -it --rm \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown start

# With persistent config
docker run -it --rm \
  -v ~/.midtown:/home/midtown/.midtown \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown start

# Status check
docker run --rm \
  -v ~/.midtown:/home/midtown/.midtown \
  -v /path/to/repo:/repo \
  -w /repo \
  btucker/midtown status
```

### Building Locally

```bash
docker build -t midtown .
docker run -it --rm -v /path/to/repo:/repo -w /repo midtown start
```

## Authentication Profiles

Midtown supports provider-scoped auth profiles (`claude`, `codex`, `zai`) so you can keep separate personal/work accounts per provider and switch quickly.

### Profile Names

Profile names can contain alphanumeric characters, hyphens, underscores, `@`, and `.`. If no profile is set, Midtown uses `default`.

### Profile Storage

Profiles are stored under `~/.midtown/`:

```text
~/.midtown/
├── auth/
│   ├── <claude-profile>/                   # Claude profile containers
│   │   └── claude/                         # CLAUDE_CONFIG_DIR (set per-session)
│   │       ├── .claude.json                # Auth tokens (per-profile, never shared)
│   │       ├── projects -> symlink         # Symlinked to shared storage
│   │       ├── tasks    -> symlink         # Symlinked to shared storage
│   │       └── ...                         # Other shared entries symlinked
│   └── providers/
│       ├── codex/
│       │   ├── current                     # Active Codex profile
│       │   └── profiles/
│       │       └── <codex-profile>/        # Codex profiles
│       └── zai/
│           ├── current                     # Active z.ai profile
│           └── profiles/
│               └── <zai-profile>/          # z.ai profiles
│                   ├── api_key.txt         # API key (chmod 600)
│                   └── base_url.txt        # Optional base URL override
└── platforms/
    └── claude/                             # Shared Claude state (all profiles)
        ├── projects/
        ├── tasks/
        └── ...
```

Claude profiles use a two-tier structure: per-profile auth credentials (`.claude.json`) live in `auth/<profile>/claude/`, while shared state (projects, tasks, settings) is symlinked from `~/.midtown/platforms/claude/`. This allows multiple auth profiles to share the same Claude Code data.

When Midtown launches a session, it sets provider-specific env vars:

- Claude: `CLAUDE_CONFIG_DIR` to the profile directory
- Codex: `CODEX_HOME` to the profile directory
- z.ai: `ANTHROPIC_AUTH_TOKEN` (API key) and `ANTHROPIC_BASE_URL` (defaults to `https://api.z.ai/api/anthropic`)

Before launching Codex, Midtown also mirrors `~/.codex/skills` into the active profile's `CODEX_HOME/skills` directory (with stale-entry cleanup) so provider profile switches don't lose installed Codex skills.

### Commands

| Command | Description |
|---------|-------------|
| `midtown auth list` | Show profile status for all supported providers (default behavior). |
| `midtown auth --provider <provider> list` | List profiles for a specific provider. |
| `midtown auth login <email>` | Create/re-authenticate a profile (prompts for provider selection). |
| `midtown auth --provider <provider> login <email>` | Create/re-authenticate a profile for a specific provider. |
| `midtown auth --provider zai login <email> --key <api-key>` | Create z.ai profile non-interactively (for scripts/CI). |
| `midtown auth switch <profile> [--project]` | Switch active profile (prompts for provider if ambiguous; global by default, use `--project` for current repo only). |
| `midtown auth --provider <provider> switch <profile> [--project]` | Switch active profile for a specific provider. |
| `midtown auth remove <profile>` | Remove a profile (prompts for provider if ambiguous). |
| `midtown auth --provider <provider> remove <profile>` | Remove a profile for a specific provider. |

### Example Workflow

```bash
# Create/login profile (prompts for provider selection)
midtown auth login work@example.com

# Create/login Claude work profile (interactive OAuth flow)
midtown auth --provider claude login work@example.com

# Create/login Codex work profile (interactive OAuth flow)
midtown auth --provider codex login work@example.com

# Create/login z.ai profile (interactive - prompts for API key)
midtown auth --provider zai login work@example.com

# Create/login z.ai profile (non-interactive - for scripts/CI)
midtown auth --provider zai login work@example.com --key sk-ant-api03-...

# Show all providers (default behavior)
midtown auth list

# List only Claude profiles
midtown auth --provider claude list

# Switch profile globally (prompts for provider if profile exists on multiple)
midtown auth switch work@example.com

# Switch Claude profile for current project only
midtown auth switch work@example.com --project

# Switch Codex profile globally
midtown auth --provider codex switch work@example.com
```

## How It Works

### Daemon

The daemon is the central coordinator. It runs an event-driven state machine that collects an immutable snapshot of the world each tick, makes pure decisions about what should happen, and then executes the resulting effects. This strict separation between decision logic and side effects keeps the core testable.

The daemon handles:

- Coworker lifecycle (spawning, health checks, stuck detection, shutdown)
- Task assignment and dispatch
- GitHub webhook processing (PR events, CI status, reviews)
- PR polling for merge conflicts and stuck conditions
- @mention routing between team members

### Coworkers

Each coworker runs as:

- A headless coding-agent process (Claude or Codex, based on provider config) managed by the daemon's `SessionManager`
- In an isolated git worktree (no merge conflicts during development)
- In a [filesystem sandbox](#sandboxing) restricting write access to project directories
- With `--add-dir` worktrees for additional repos in multi-repo projects
- Nudges are delivered via stdin JSON, and health is monitored via stdout stream events

Coworkers are named after Manhattan avenues: lexington, park, madison, broadway, amsterdam, columbus, riverside, york, pleasant, vernon.

### Channel Leads

Each topic channel can have a **channel lead** — a headless Claude Code session that acts as a domain expert for that channel. Channel leads are **on-demand**: they spawn when triggered by a user message, task creation, or insight in their channel, and idle-shutdown like regular coworkers when inactive. When woken, they resume their previous session if one exists from the current daemon run, or start fresh with the trigger context baked into their initial prompt.

**What channel leads do:**

- Brainstorm with the user on domain topics in their channel
- Maintain living design documents, architecture notes, and decision logs
- Answer domain questions with accumulated context
- Receive user messages posted to their topic channel

**What channel leads don't do:** Channel leads don't write code, open PRs, or create tasks. When implementation work is needed, they escalate to @lead.

**Forked sessions:** Channel leads can fork themselves into thread-specific sessions via `midtown session fork <thread-id>`. A forked session inherits the parent's conversation context but gets an independent session ID bound to a specific thread. Thread replies are automatically routed to the fork, and the fork's channel posts are auto-tagged with the bound thread ID. For messages requiring investigation, the channel lead posts a brief acknowledgment first (so the user sees immediate feedback), then forks — the fork handles the rest of the conversation.

When a forked session creates a task, it can pass `--thread-id <message-id>` to `midtown task create` (the CLI automatically uses `$MIDTOWN_BOUND_THREAD_ID` inside forked sessions). The daemon stores that binding with the task so the coworker spawned for it posts updates back into the originating thread, even across restarts or session resumes.

Coworkers use `@{channel-name}` for domain questions (e.g., `@auth-refactor can you explain the token expiry logic?`) and reserve `@lead` for coordination and priority questions.

### Workflow Scripts

Each channel can have a `workflow.py` script that customizes how the daemon responds to events — PR lifecycle, coworker status changes, task transitions, CI results, and more. Scripts are invoked by the daemon via `uv run` using the [Midtown Python SDK](sdk/python/).

**Script resolution order** (first file found wins):

1. `<project_root>/.midtown/channels/<channel>/workflow.py` — channel-specific, committed to repo
2. `~/.midtown/projects/<repo>/channels/<channel>/workflow.py` — channel-specific, local only
3. `<project_root>/.midtown/workflow.py` — project default, committed to repo
4. `~/.midtown/projects/<repo>/workflow.py` — project default, local only

If no script is found, the daemon falls back to its compiled-in default behavior. **Changes take effect on the next daemon tick** — no restart needed.

To bootstrap a channel-specific script from the reference implementation:

```bash
mkdir -p .midtown/channels/<channel>
cp $(python -c "import midtown, os; print(os.path.dirname(midtown.__file__))")/default_workflow.py \
   .midtown/channels/<channel>/workflow.py
```

The reference implementation (`sdk/python/midtown/default_workflow.py`) replicates the built-in PR lifecycle using a state machine. Customize it to add channel-specific automation.

### Channel Sync

Coworkers stay synchronized via a Claude Code Stop hook. When Claude pauses, the hook reads new channel messages and checks for unclaimed tasks. This means coworkers automatically receive updates at natural pause points.

### Mailbox Messaging

In addition to the shared channel, the daemon can deliver targeted messages to individual coworkers via the Claude Code agent teams mailbox protocol. Messages are written as JSON to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json` using atomic file operations with mkdir-based locking for safe concurrent access.

### Worktree Lifecycle

When a coworker is called in, midtown creates a detached git worktree at the current HEAD. The coworker creates a feature branch and works independently. When the coworker shuts down, worktrees with no commits and no uncommitted changes are automatically cleaned up along with their branches. Worktrees with work in progress are preserved.

### GitHub Integration

The daemon receives real-time GitHub events via webhooks (PR creation, reviews, check runs) verified with HMAC-SHA256 signatures. PR polling runs as a backstop for missed webhook deliveries and handles time-based concerns like merge conflict detection and stuck PR identification.

### Webhook Ports

Each project daemon runs its own webhook server for GitHub integration. Port 47022 is reserved for the shared multi-project webserver. Per-project daemons auto-assign ports starting at 47023, persisting the assignment in the project's `config.toml` for stability across restarts.

### Chat TUI

The `midtown chat` command opens a split-panel interface with:

**Layout**:

- **Board panel** (left 40%): Channel swimlanes showing in-progress (●) and pending (○) tasks per channel
- **Chat panel** (right 60%): Real-time message display with mermaid diagram rendering
- **Input bar** (bottom): Text input for posting messages (Tab to focus, Enter to send)

**Features**:

- Real-time channel message display
- Mermaid diagram detection and rendering (via `selkie-rs` with content-hash caching)
- Inline ASCII art for flowchart diagrams (press number keys to open SVG in browser)
- **Type-anywhere UX**: Character keys auto-focus the input bar (like Slack/Discord)
- **Ctrl+K quick channel switcher**: Slack-style overlay for fast channel navigation with real-time filtering
- Tab-based focus navigation (Board → Chat → InputBar)
- Arrow keys, PageUp/PageDown, Home/End for scrolling
- Ctrl+A to toggle archived channel visibility
- Mouse support for scrolling and navigation
- Clickable hyperlinks via OSC 8 escape sequences
- Real-time token usage and cost tracking

### Web UI

The web interface is a Svelte 5 + Vite SPA served on port 47022:

- Installable as a PWA for mobile use
- Real-time updates via WebSocket
- Channel-grouped task sidebar for task visualization
- Multi-channel support with split-panel layout (channel list sidebar + message pane)
- Channel list with task counts (in progress, pending) and CI status badges
- Channel header displays channel-specific stats (PR count, in-progress tasks, pending tasks) that update when switching channels
- Create new channels directly from the sidebar (+ button) with inline validation
- Clickable channel (`#name`), task (`!N`), and PR (`#N`) references in messages
- Insight cross-post highlighting with source channel attribution
- Mermaid diagram rendering in chat messages
- Image and document paste support (clipboard → inline preview → upload to lead)
- Coworker status monitoring
- Auth profile switching
- Push notifications (W3C Push API with VAPID)
- Responsive layout with three breakpoints:
  - **Mobile (≤768px)**: Tab navigation, hamburger menu with slide-out sidebar, modal popups for task/PR details
  - **Tablet (769–1024px)**: Permanent sidebar replaces tab navigation, two-column grid layout
  - **Desktop (≥1025px)**: Three-column Slack-inspired layout with sidebar, main channel, and toggleable detail panel for tasks, PRs, and coworker info
- Clickable `@coworker` mentions in messages open coworker detail panel on desktop

### Reminders

The Lead can set reminders that trigger on specific conditions:

```bash
# Remind me when all tasks are done and PRs merged
midtown lead remind all-work-merged "Time to deploy!"

# List active reminders
midtown lead remind list

# Cancel a reminder
midtown lead remind cancel <id>
```

Reminders are stored in `~/.midtown/projects/<repo>/reminders.json` and evaluated by the daemon each tick.

### Sandboxing

Midtown uses platform-native filesystem sandboxing to restrict write access for both the Lead session and all Coworkers, while preserving full read access:

- **macOS**: `sandbox-exec` with SBPL profiles
- **Linux**: `bwrap` (bubblewrap) when available; falls back to unsandboxed execution if bwrap is not installed
- **Zero overhead**: Same-host sandboxing — no Docker, no containers, same binaries and auth tokens

**Fallback behavior:** If sandbox setup fails (e.g., missing `bwrap` on Linux, profile creation errors on macOS, or sandbox nesting detected after `midtown restart`), Midtown logs a warning and falls back to running without the extra sandbox wrapper. When nesting is detected, coworkers still inherit the Lead's sandbox through the process chain, so write restrictions remain in effect. This ensures sessions aren't blocked by sandbox issues.

**How it works:**

The sandbox allows reads from anywhere but restricts writes to explicitly permitted directories:

- Project repositories (primary + additional repos in multi-repo projects)
- `~/.midtown` (daemon state, channel logs, worktrees)
- `~/.claude` (Claude Code config, sessions, tasks)
- `~/.codex` (Codex config)
- `~/.local/state/midtown` (daemon socket, runtime state)
- `/tmp` and platform-specific temp directories
- Additional paths configured via `[sandbox].allowed_paths` (see below)

**Configuring additional writable paths:**

You can add extra writable directories to the sandbox via config. This is useful for tools that need to write outside the project directory (e.g., `~/.cargo` for Rust tooling, `~/.npm` for Node.js).

Global config (`~/.midtown/config.toml`):

```toml
[sandbox]
allowed_paths = ["~/.cargo", "~/.rustup"]
```

Project-level config (`~/.midtown/projects/<repo>/config.toml`):

```toml
[sandbox]
allowed_paths = ["/opt/toolchain"]
```

Project-level paths **extend** (not replace) global paths. Paths support `~` expansion and are automatically deduplicated.

**Git worktree support:** When the primary repo is a git worktree, Midtown detects this and automatically adds the main repository's `.git/` directory to the writable list, ensuring git operations (commits, refs, objects) work correctly.

**macOS SBPL profile structure:**

```scheme
(allow default)                          ; Allow all operations by default
(deny file-write* (subpath "$HOME"))    ; Block writes under home directory
(allow file-write*                       ; Re-allow writes to specific paths
  (subpath "/path/to/project")
  (subpath "~/.midtown")
  ...)
```

This approach means Claude Code can access any file for context (reading documentation, analyzing dependencies) but can only modify files within the project workspace and configuration directories.

## License

MIT
