> Back to [README](../README.md)

# Configuration

Midtown uses two levels of config files:

1. **Global config** at `~/.midtown/config.toml` — applies to all projects
2. **Project config** at `~/.midtown/projects/<project>/config.toml` — overrides per project

Project settings take precedence over global defaults. All fields are optional.

## Global `config.toml`

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
channel_lead_provider = "codex"       # Optional channel-lead override (falls back to lead_provider)
specialized_provider = "claude"       # Default for specialized workers
architect_provider = "zai"            # Optional override (falls back to specialized_provider)
headless_execute_provider = "claude"  # Optional override (falls back to specialized_provider)
```

## Project `config.toml`

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
  - Architect: `execution.architect_provider` → `execution.specialized_provider` → `claude`
  - `oneshot.execute`: `execution.headless_execute_provider` → `execution.specialized_provider` → `claude`

Model aliases are auto-normalized per provider at launch:

- Generic sizes:
  - Claude: `small` → `haiku`, `medium` → `sonnet`, `large` → `opus`
  - z.ai: `small` → `GLM-4.5-Air`, `medium` → `GLM-4.7`, `large` → `GLM-5`
  - Codex: `small` → `gpt-5.1-codex-mini`, `medium` → `gpt-5.3-codex-spark`, `large` → `gpt-5.3-codex`
- Cross-provider safety:
  - Claude/z.ai aliases (`haiku`/`sonnet`/`opus`) are normalized to Codex defaults when provider is Codex.
  - `gpt-5-codex` is normalized to role defaults (`opus` for lead/reviewer, `sonnet` for coworker/channel lead) when provider is Claude/z.ai.

## Environment Variable Overrides

Daemon settings can be overridden with environment variables:

| Variable | Overrides |
|----------|-----------|
| `MIDTOWN_WEBHOOK_PORT` | `webhook_port` |
| `MIDTOWN_WEBHOOK_SECRET` | `webhook_secret` |
| `MIDTOWN_WEBHOOK_RESTART_INTERVAL` | `webhook_restart_interval_secs` |
| `MIDTOWN_PR_POLL_INTERVAL` | `pr_poll_interval_secs` |
| `MIDTOWN_CHAT_MONITOR` | `chat_monitor_enabled` (set to `0` to disable) |
| `MIDTOWN_MAX_COWORKERS` | `max_coworkers` |

## Custom System Prompts

Customize the system prompts for Lead and Coworkers with markdown files:

- `~/.midtown/LEAD.md` / `~/.midtown/COWORKER.md` - Global custom prompts
- `~/.midtown/projects/<project>/LEAD.md` / `COWORKER.md` - Per-project custom prompts

Content from these files is appended to the built-in system prompts. Project-level files supplement global ones.
