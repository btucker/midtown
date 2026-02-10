<div align="center"><img src="docs/images/logo.png" width="100" alt="Midtown logo"></div>

# Midtown

Work with a "lead" to manage your team of **Claude Code** "coworkers" to accomplish tasks following a github PR kanban workflow.

## Why Midtown?

Midtown is inspired by [Gastown](https://github.com/anthropics/claude-code/tree/main/.claude/docs/gastown.md), but a bit simpler, less exciting, and, well, more mid.

At its core, Midtown is built around an IRC-like messaging model: a shared channel where team members (both the human-facing "Lead" and autonomous "Coworkers") post updates, coordinate handoffs, and stay in sync. This append-only message stream is the backbone of multi-agent collaboration—each Claude Code instance reads the channel at natural pause points, just like checking a team chat. At anytime the "lead" or a "coworker" can be @mentioned and then receive that into their context context immediately.

When you're working with Claude Code on a complex project, you might want to parallelize work:

- The Lead collaborates with the human to create a plan & split up the work into tasks & dependencies.
- Multiple "coworkers" implement independent components simultaneously but with context of the full project
- The "coworkers"" review & merge PRs while the "lead"" & human collaborate on what's next

Midtown provides two UIs:

1. A tmux-based TUI with an IRC-style chat pane (includes mermaid diagram rendering with inline ASCII art)
2. A web interface (meant to be run as a PWA) so you can collaborate with the lead (and the team) while on the go.

Midtown makes extensive use of the new Claude Code Tasks system to manage the state of all work, create dependencies, and assign ownership.

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

This starts the daemon and creates a tmux session with the Lead window.

For multi-repo projects, specify a project name and additional repos:

```bash
midtown start --project myapp --add-repo /path/to/frontend --add-repo /path/to/shared-lib
```

### 3. Attach to the session

If you're in a repo that's part of a project:

```bash
midtown attach
```

You're now in the Lead's Claude Code instance.

To attach to a named project from any directory:

```bash
midtown attach myapp
```

### 4. Work with the lead as you typically would work with Claude Code

The lead is just a claude code session, but it's been booted with some a [special system prompt](agents/lead.md). The system prompt instructs the lead how to execute in the midtown environment-- mostly to not take on work itself (unless it's trivial) and to instead make Claude Code tasks.

## CLI Overview

| Command | Description |
|---------|-------------|
| `midtown start` | Start the daemon and tmux session |
| `midtown stop` | Stop everything |
| `midtown restart` | Restart the daemon |
| `midtown attach` | Attach to the tmux session |
| `midtown status` | Show system status |
| `midtown chat` | Open the IRC-style chat TUI |
| `midtown log [-f]` | View daemon logs |
| `midtown auth` | Manage [auth profiles](docs/authentication.md) (multiple accounts supported) |
| `midtown headless "<prompt>"` | Run a headless Claude session |

See the [full CLI reference](docs/cli.md) for all flags and options.

## Documentation

| Topic | Description |
|-------|-------------|
| [CLI Reference](docs/cli.md) | Full command reference with all flags and agent-internal commands |
| [Configuration](docs/configuration.md) | Global and per-project config files, environment variables, custom prompts |
| [Authentication](docs/authentication.md) | Multi-account auth profiles, switching, storage |
| [Architecture](docs/architecture.md) | Daemon, coworkers, channel sync, GitHub integration, web UI |
| [Docker](docs/docker.md) | Docker images, running in containers |

## License

MIT
