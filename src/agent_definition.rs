//! Agent definition file loading and parsing.
//!
//! Loads agent definitions from markdown files with YAML frontmatter.
//! These files follow the Claude Code agent definition format:
//!
//! ```markdown
//! ---
//! name: my-agent
//! description: What this agent does
//! model: opus
//! ---
//!
//! System prompt content goes here...
//! ```
//!
//! Search paths (in order):
//! 1. `.claude/agents/{name}.md` (project-level)
//! 2. `~/.claude/agents/{name}.md` (user-level)

use std::path::{Path, PathBuf};

/// Parsed agent definition from a markdown file.
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// Agent name from frontmatter.
    pub name: String,
    /// Agent description from frontmatter.
    pub description: Option<String>,
    /// Model override (e.g., "opus", "sonnet").
    pub model: Option<String>,
    /// Lucide icon name for avatar badge (e.g., "pen-line", "search").
    pub avatar_badge: Option<String>,
    /// The markdown body — used as the agent's system prompt.
    pub system_prompt: String,
    /// Path the definition was loaded from (for diagnostics).
    pub source_path: PathBuf,
}

/// Load an agent definition by name, searching project then user directories.
///
/// Returns the first matching definition found, or an error describing
/// which paths were checked.
pub fn load_agent_definition(name: &str) -> Result<AgentDefinition, String> {
    let candidates = agent_definition_paths(name);

    for path in &candidates {
        if path.exists() {
            return parse_agent_file(path);
        }
    }

    Err(format!(
        "Agent definition '{}' not found. Searched:\n{}",
        name,
        candidates
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// Return the candidate file paths for an agent definition, in search order.
pub fn agent_definition_paths(name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. Project-level: .claude/agents/{name}.md
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(
            cwd.join(".claude")
                .join("agents")
                .join(format!("{}.md", name)),
        );
    }

    // 2. User-level: ~/.claude/agents/{name}.md
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join(".claude")
                .join("agents")
                .join(format!("{}.md", name)),
        );
    }

    paths
}

/// Parse a single agent definition file.
///
/// Expects YAML frontmatter delimited by `---` lines, followed by a markdown body.
pub fn parse_agent_file(path: &Path) -> Result<AgentDefinition, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

    parse_agent_content(&content, path)
}

/// Strip surrounding YAML quotes (`"` or `'`) from a value string.
fn strip_yaml_quotes(s: &str) -> String {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parse agent definition content (testable without filesystem).
pub(crate) fn parse_agent_content(
    content: &str,
    source_path: &Path,
) -> Result<AgentDefinition, String> {
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        // No frontmatter — treat entire content as system prompt, use filename as name
        let name = source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        return Ok(AgentDefinition {
            name,
            description: None,
            model: None,
            avatar_badge: None,
            system_prompt: content.to_string(),
            source_path: source_path.to_path_buf(),
        });
    }

    // Find the closing --- delimiter
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);

    let closing_pos = after_first.find("\n---").ok_or_else(|| {
        format!(
            "No closing '---' in frontmatter of {}",
            source_path.display()
        )
    })?;

    let frontmatter_str = &after_first[..closing_pos];
    let body_start = closing_pos + 4; // skip "\n---"
    let body = after_first[body_start..].trim_start_matches(['\r', '\n']);

    // Parse frontmatter as YAML-like key: value pairs (simple parser — no serde_yaml dependency)
    let mut name = None;
    let mut description = None;
    let mut model = None;
    let mut avatar_badge = None;

    for line in frontmatter_str.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = strip_yaml_quotes(value.trim());
            match key {
                "name" => name = Some(value.to_string()),
                "description" => description = Some(value.to_string()),
                "model" => {
                    if !value.is_empty() {
                        model = Some(value.to_string());
                    }
                }
                "avatar_badge" => {
                    if !value.is_empty() {
                        avatar_badge = Some(value.to_string());
                    }
                }
                _ => {} // Ignore unknown fields (tools, etc.)
            }
        }
    }

    let resolved_name = name.unwrap_or_else(|| {
        source_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    Ok(AgentDefinition {
        name: resolved_name,
        description,
        model,
        avatar_badge,
        system_prompt: body.to_string(),
        source_path: source_path.to_path_buf(),
    })
}

#[path = "agent_definition_tests.rs"]
#[cfg(test)]
mod tests;
