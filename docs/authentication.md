> Back to [README](../README.md)

# Authentication Profiles

Midtown supports multiple Claude authentication profiles, allowing you to switch between different Claude accounts (e.g., personal and work accounts) without re-authenticating each time.

## Profile Names

Profile names are email addresses (e.g., `user@example.com`). They can contain alphanumeric characters, hyphens, underscores, `@`, and `.`. If no profile is specified, commands default to a profile named `default`.

## Profile Storage

Profiles are stored in `~/.midtown/auth/`:

```
~/.midtown/auth/
├── current              # Text file containing the active profile name
└── <profile>/           # Per-profile directory
    └── .claude.json     # Claude config with auth tokens
```

When midtown spawns Claude sessions (Lead or Coworkers), it sets `CLAUDE_CONFIG_DIR` to the active profile's directory, isolating authentication between profiles.

Per-project profile overrides are stored in the project's `config.toml`:

```toml
# ~/.midtown/projects/<project>/config.toml
[project]
auth_profile = "work@example.com"
```

## Commands

| Command | Description |
|---------|-------------|
| `midtown auth login <email>` | Create a new profile or re-authenticate an existing one. Launches a Claude session where you run `/login` to complete OAuth. |
| `midtown auth list` | List all available profiles with usage data, and interactively switch between them. |
| `midtown auth switch <profile> [--all]` | Switch to a different profile. Without `--all`, switches for the current project only. With `--all`, switches globally and clears per-project overrides. |
| `midtown auth remove <profile>` | Remove a profile and its stored credentials. |

## Example Workflow

```bash
# Set up a work profile
midtown auth login work@example.com
# Claude opens — run /login inside to authenticate

# Set up a personal profile
midtown auth login personal@example.com
# Claude opens — run /login inside to authenticate

# List profiles (interactive TUI selector)
midtown auth list
# Shows profiles sorted by available capacity with usage data
# Use arrow keys to switch, Del to remove, Enter to confirm

# Switch to work account (current project only)
midtown auth switch work@example.com

# Switch to work account (all projects)
midtown auth switch work@example.com --all
```

## Running Claude with Auth

Use `midtown claude` to run the Claude CLI with your active profile's credentials:

```bash
midtown claude --version
midtown claude -p "Summarize this file" src/main.rs
```

This sets `CLAUDE_CONFIG_DIR` automatically so Claude uses the correct auth tokens.
