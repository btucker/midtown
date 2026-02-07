//! Headless architect for generating Mermaid diagrams from insights.
//!
//! When a coworker or the lead reports an insight, the daemon can optionally
//! spawn a headless Claude session (the "architect") that explores the relevant
//! code and produces a Mermaid diagram illustrating the insight. The diagram is
//! then posted to the channel as a follow-up message from the virtual `architect`
//! agent.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::headless::{HeadlessConfig, execute};
use crate::message::Message;

/// Maximum number of concurrent architect sessions.
const MAX_CONCURRENT_SESSIONS: usize = 2;

/// Timeout for a single architect session.
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

/// Semaphore to limit concurrent architect sessions.
/// Prevents resource exhaustion when many insights arrive in quick succession.
static ARCHITECT_SEMAPHORE: Semaphore = Semaphore::const_new(MAX_CONCURRENT_SESSIONS);

// Compile-time invariants for config constants
const _: () = assert!(MAX_CONCURRENT_SESSIONS >= 1);
const _: () = assert!(MAX_CONCURRENT_SESSIONS <= 4);
const _: () = assert!(SESSION_TIMEOUT.as_secs() >= 60);
const _: () = assert!(SESSION_TIMEOUT.as_secs() <= 300);

const ARCHITECT_SYSTEM_PROMPT: &str = r#"You are an architectural diagram illustrator for a software project. You receive an insight about the codebase and have full tool access to explore the code.

Your job:
1. Evaluate whether a diagram would genuinely help illustrate this insight
2. If yes, explore the relevant source files to understand the structure
3. Produce a single Mermaid diagram that accurately represents the insight
4. Validate the diagram renders correctly using selkie (see below)
5. If a diagram adds no value (e.g., the insight is about a simple naming choice), return exactly: NO_DIAGRAM

Validation — REQUIRED before returning any diagram:
After generating your mermaid diagram, you MUST validate it renders by running:
  echo '<your mermaid source>' | midtown diagram validate
If the command fails, read the error message, fix the diagram syntax, and re-validate.
You have at most 2 fix attempts. If you still cannot produce a valid diagram after 2 fixes, return NO_DIAGRAM.
Only return the ```mermaid fence block after it passes validation.

Rules:
- Output exactly one ```mermaid fence block, or NO_DIAGRAM — nothing else
- Keep diagrams focused: max 10-15 nodes
- Use the actual code to ensure accuracy — read files, don't guess
- Prefer flowcharts for data/control flow, sequence diagrams for interactions, class diagrams for structure
- Label nodes with real function/struct/module names from the code
- No decorative elements — every node should convey information"#;

/// Spawn a headless architect session to generate a diagram for an insight.
///
/// This is designed to be called from `tokio::spawn` — it runs independently
/// and posts the result to the channel. Errors are logged and silently skipped;
/// the insight itself has already been posted before this function runs.
pub async fn generate_insight_diagram(insight: String, cwd: PathBuf, repo_name: String) {
    // Limit concurrent architect sessions to prevent resource exhaustion.
    // If all permits are taken, skip this diagram rather than queuing up.
    let _permit = match ARCHITECT_SEMAPHORE.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            info!(
                "Architect: skipping diagram — {} concurrent sessions already running",
                MAX_CONCURRENT_SESSIONS
            );
            return;
        }
    };

    let cwd_str = cwd.to_string_lossy().to_string();

    let config = HeadlessConfig {
        model: "sonnet".to_string(),
        system_prompt: ARCHITECT_SYSTEM_PROMPT.to_string(),
        json_schema: None,
        cwd: Some(cwd_str.clone()),
        max_budget_usd: Some(0.50),
        allow_tools: true,
        persist_session: false,
        resume_session_id: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        settings_path: None,
    };

    info!(
        "Architect: generating diagram for insight (cwd={})",
        cwd_str
    );

    let result = match execute(&config, &insight, SESSION_TIMEOUT).await {
        Ok(r) => r,
        Err(e) => {
            warn!("Architect: headless execution failed: {}", e);
            return;
        }
    };

    if result.is_error {
        warn!("Architect: session returned error");
        return;
    }

    let Some(output) = result.result else {
        warn!("Architect: no result text returned");
        return;
    };

    info!(
        "Architect: session complete (cost=${:.4}, duration={}ms)",
        result.cost_usd.unwrap_or(0.0),
        result.duration_ms.unwrap_or(0),
    );

    // Check for NO_DIAGRAM response
    if output.trim() == "NO_DIAGRAM" {
        info!("Architect: no diagram needed for this insight");
        return;
    }

    // Extract mermaid fence block from the output
    let Some(diagram) = extract_mermaid_block(&output) else {
        info!("Architect: no mermaid block found in output, skipping");
        return;
    };

    // Safety net: verify the diagram renders with selkie. The architect should
    // have already validated via CLI, but LLMs don't always follow instructions.
    if let Err(e) = selkie::render::render_text(&diagram) {
        warn!(
            "Architect: diagram failed selkie validation (architect may have skipped CLI check): {}",
            e
        );
        return;
    }

    // Post diagram to channel as "architect"
    let channel = match crate::Channel::for_repo(&repo_name) {
        Ok(ch) => ch,
        Err(e) => {
            warn!("Architect: failed to open channel: {}", e);
            return;
        }
    };
    let msg = Message::text("architect", format!("```mermaid\n{}\n```", diagram));
    if let Err(e) = channel.send(&msg) {
        warn!("Architect: failed to post diagram to channel: {}", e);
    } else {
        info!("Architect: posted diagram to channel");
    }
}

/// Extract the content of the first ```mermaid fence block from text.
fn extract_mermaid_block(text: &str) -> Option<String> {
    let start_marker = "```mermaid";
    let end_marker = "```";

    let start = text.find(start_marker)?;
    let content_start = start + start_marker.len();

    // Skip the newline after the opening fence
    let content_start = if text[content_start..].starts_with('\n') {
        content_start + 1
    } else {
        content_start
    };

    // Find the closing fence (must be after the content start)
    let end = text[content_start..].find(end_marker)?;
    let content = text[content_start..content_start + end].trim();

    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_mermaid_block_basic() {
        let text = r#"```mermaid
graph TD
    A[Start] --> B[End]
```"#;
        let result = extract_mermaid_block(text);
        assert_eq!(
            result,
            Some("graph TD\n    A[Start] --> B[End]".to_string())
        );
    }

    #[test]
    fn test_extract_mermaid_block_with_surrounding_text() {
        let text = r#"Here is the diagram:

```mermaid
sequenceDiagram
    A->>B: Hello
```

Done."#;
        let result = extract_mermaid_block(text);
        assert_eq!(
            result,
            Some("sequenceDiagram\n    A->>B: Hello".to_string())
        );
    }

    #[test]
    fn test_extract_mermaid_block_no_diagram() {
        let text = "NO_DIAGRAM";
        assert!(extract_mermaid_block(text).is_none());
    }

    #[test]
    fn test_extract_mermaid_block_empty_fence() {
        let text = "```mermaid\n```";
        assert!(extract_mermaid_block(text).is_none());
    }

    #[test]
    fn test_extract_mermaid_block_no_fence() {
        let text = "Just some regular text without any mermaid blocks.";
        assert!(extract_mermaid_block(text).is_none());
    }

    #[test]
    fn test_valid_mermaid_passes_selkie_validation() {
        let diagram = "graph TD\n    A[Start] --> B[End]";
        assert!(
            selkie::render::render_text(diagram).is_ok(),
            "valid mermaid should pass selkie validation"
        );
    }

    #[test]
    fn test_invalid_mermaid_fails_selkie_validation() {
        let diagram = "not valid mermaid {{{";
        assert!(
            selkie::render::render_text(diagram).is_err(),
            "invalid mermaid should fail selkie validation"
        );
    }

    #[test]
    fn test_system_prompt_contains_validation_instructions() {
        assert!(
            ARCHITECT_SYSTEM_PROMPT.contains("midtown diagram validate"),
            "system prompt should include midtown diagram validate command"
        );
        assert!(
            ARCHITECT_SYSTEM_PROMPT.contains("2 fix attempts"),
            "system prompt should cap retries at 2"
        );
    }
}
