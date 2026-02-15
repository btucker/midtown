//! Pane lifecycle and nudge delivery actions for coworker sessions.
//!
//! Phase 4 additions to the plugin: Lead pane detection from PaneManifest,
//! nudge delivery via `write_chars_to_pane_id`, attached pane tracking
//! via CommandPaneOpened/CommandPaneExited events, and auto-detach when
//! the coworker pane closes.

use std::collections::BTreeMap;

use zellij_tile::prelude::*;

use crate::state::PluginState;

/// Handle a `CommandPaneOpened` event.
///
/// When we open a command pane for an attached coworker, Zellij notifies us
/// with the pane ID. We store it so we can track and close it later.
pub fn handle_command_pane_opened(
    state: &mut PluginState,
    pane_id: u32,
    context: &BTreeMap<String, String>,
) {
    if context.get("midtown_attached").map(|v| v.as_str()) == Some("true") {
        state.attached_pane_id = Some(pane_id);
    }
}

/// Handle a `CommandPaneExited` event.
///
/// When an attached coworker pane exits (user types /exit or process ends),
/// trigger automatic detach.
pub fn handle_command_pane_exited(
    state: &mut PluginState,
    pane_id: u32,
    context: &BTreeMap<String, String>,
) -> Option<String> {
    if context.get("midtown_attached").map(|v| v.as_str()) == Some("true")
        && state.attached_pane_id == Some(pane_id)
    {
        state.attached_pane_id = None;
        // Return the coworker name so the caller can trigger detach RPC
        state.attached_coworker_name().map(|n| n.to_string())
    } else {
        None
    }
}

/// Deliver queued nudges to the Lead's terminal pane.
///
/// Writes each nudge message to the Lead's pane STDIN, followed by Enter.
/// This replaces the tmux `send-keys` mechanism for nudge delivery.
pub fn deliver_nudges(state: &PluginState, nudges: &[String]) {
    if nudges.is_empty() {
        return;
    }

    let lead_pane_id = match state.lead_pane_id {
        Some(id) => id,
        None => return, // Can't deliver nudges without knowing the Lead's pane
    };

    for nudge in nudges {
        // Write the nudge text to the Lead's pane STDIN
        write_chars_to_pane_id(nudge, lead_pane_id);
        // Send Enter key (carriage return byte)
        write_to_pane_id(vec![13], lead_pane_id);
    }
}

fn lead_command_markers(provider: Option<&str>) -> &'static [&'static str] {
    match provider.unwrap_or("claude").to_ascii_lowercase().as_str() {
        "codex" => &["codex"],
        // z.ai runs through the Claude CLI with Anthropic-compatible env vars.
        "zai" => &["claude"],
        // Safe default for missing/unknown provider.
        _ => &["claude"],
    }
}

fn lead_command_matches(cmd: &str, provider: Option<&str>) -> bool {
    let cmd = cmd.to_ascii_lowercase();
    lead_command_markers(provider)
        .iter()
        .any(|marker| cmd.contains(marker))
}

/// Detect the Lead's pane from a PaneManifest update.
///
/// The Lead's pane is identified by matching the provider-specific command
/// marker (`claude` for Claude/z.ai, `codex` for Codex) and not being an
/// attached coworker pane.
///
/// Heuristic: Find the largest non-plugin, non-attached terminal pane running
/// the expected provider command. If no command-match pane is found, use the
/// focused terminal pane as fallback.
pub fn detect_lead_pane(state: &mut PluginState, pane_manifest: &PaneManifest) {
    let mut best_candidate: Option<(u32, usize)> = None; // (pane_id, area)
    let mut focused_terminal: Option<u32> = None;
    let lead_provider = state.lead_provider.as_deref();

    for panes in pane_manifest.panes.values() {
        for pane in panes {
            // Skip plugin panes
            if pane.is_plugin {
                // Track our own plugin pane ID
                if let Some(url) = &pane.plugin_url
                    && url.contains("midtown")
                {
                    state.self_pane_id = Some(pane.id);
                }
                continue;
            }

            // Skip attached coworker pane
            if Some(pane.id) == state.attached_pane_id {
                continue;
            }

            // Track focused terminal pane as fallback
            if pane.is_focused {
                focused_terminal = Some(pane.id);
            }

            // Look for panes running the configured Lead provider command.
            if let Some(cmd) = &pane.terminal_command
                && lead_command_matches(cmd, lead_provider)
            {
                let area = pane.pane_rows * pane.pane_columns;
                if best_candidate.is_none_or(|(_, best_area)| area > best_area) {
                    best_candidate = Some((pane.id, area));
                }
            }
        }
    }

    // Use the best provider-matching pane, or fall back to focused terminal.
    if let Some((pane_id, _)) = best_candidate {
        state.lead_pane_id = Some(PaneId::Terminal(pane_id));
    } else if let Some(pane_id) = focused_terminal {
        // Only set fallback if we don't already have a lead pane
        if state.lead_pane_id.is_none() {
            state.lead_pane_id = Some(PaneId::Terminal(pane_id));
        }
    }
}

/// Detect if an attached coworker's pane was closed.
///
/// When a user closes an attached coworker pane (e.g., via /exit or closing the pane),
/// we need to detect this and trigger a detach RPC to resume the headless session.
///
/// Returns the name of the coworker that needs detaching, if any.
pub fn detect_closed_attached_pane(
    state: &PluginState,
    pane_manifest: &PaneManifest,
) -> Option<String> {
    // Only check if we have an attached pane
    let attached_pane_id = state.attached_pane_id?;
    let attached_name = state.attached_coworker_name()?;

    // Check if the attached pane still exists in the manifest
    let pane_exists = pane_manifest.panes.values().any(|panes| {
        panes
            .iter()
            .any(|p| p.id == attached_pane_id && !p.is_plugin)
    });

    if !pane_exists {
        return Some(attached_name.to_string());
    }

    // Also check if the pane has exited
    let pane_exited = pane_manifest.panes.values().any(|panes| {
        panes
            .iter()
            .any(|p| p.id == attached_pane_id && !p.is_plugin && p.exited)
    });

    if pane_exited {
        Some(attached_name.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lead_command_matches_claude_default() {
        assert!(lead_command_matches("claude --resume abc", None));
        assert!(lead_command_matches("CLAUDE --resume abc", Some("claude")));
        assert!(!lead_command_matches("codex resume abc", Some("claude")));
    }

    #[test]
    fn test_lead_command_matches_codex_provider() {
        assert!(lead_command_matches("codex resume abc", Some("codex")));
        assert!(!lead_command_matches("claude --resume abc", Some("codex")));
    }

    #[test]
    fn test_lead_command_matches_zai_uses_claude_cli() {
        assert!(lead_command_matches("claude --resume abc", Some("zai")));
        assert!(!lead_command_matches("codex resume abc", Some("zai")));
    }
}
