use std::path::Path;

use super::*;

#[test]
fn test_parse_full_frontmatter() {
    let content = r#"---
name: tdw-critic
description: Reviews code for test quality
model: opus
---

You are an expert test reviewer.
Focus on coverage gaps."#;

    let def = parse_agent_content(content, Path::new("/tmp/tdw-critic.md")).unwrap();
    assert_eq!(def.name, "tdw-critic");
    assert_eq!(
        def.description.as_deref(),
        Some("Reviews code for test quality")
    );
    assert_eq!(def.model.as_deref(), Some("opus"));
    assert_eq!(
        def.system_prompt,
        "You are an expert test reviewer.\nFocus on coverage gaps."
    );
}

#[test]
fn test_parse_minimal_frontmatter() {
    let content = r#"---
name: simple-agent
---

Do simple things."#;

    let def = parse_agent_content(content, Path::new("/tmp/simple-agent.md")).unwrap();
    assert_eq!(def.name, "simple-agent");
    assert!(def.description.is_none());
    assert!(def.model.is_none());
    assert_eq!(def.system_prompt, "Do simple things.");
}

#[test]
fn test_parse_no_frontmatter() {
    let content = "Just a system prompt with no frontmatter.";

    let def = parse_agent_content(content, Path::new("/tmp/my-agent.md")).unwrap();
    assert_eq!(def.name, "my-agent");
    assert!(def.description.is_none());
    assert!(def.model.is_none());
    assert_eq!(def.system_prompt, content);
}

#[test]
fn test_parse_empty_model_ignored() {
    let content = r#"---
name: test
model:
---

Prompt."#;

    let def = parse_agent_content(content, Path::new("/tmp/test.md")).unwrap();
    assert!(def.model.is_none(), "Empty model should be None");
}

#[test]
fn test_parse_missing_closing_delimiter() {
    let content = r#"---
name: broken
no closing delimiter"#;

    let result = parse_agent_content(content, Path::new("/tmp/broken.md"));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("No closing '---'"));
}

#[test]
fn test_parse_name_falls_back_to_filename() {
    let content = r#"---
description: No name field
---

Prompt body."#;

    let def = parse_agent_content(content, Path::new("/tmp/fallback-name.md")).unwrap();
    assert_eq!(def.name, "fallback-name");
}

#[test]
fn test_parse_avatar_badge() {
    let content = r#"---
name: test-agent
avatar_badge: pen-line
---

Prompt."#;

    let def = parse_agent_content(content, Path::new("/tmp/test-agent.md")).unwrap();
    assert_eq!(def.name, "test-agent");
    assert_eq!(def.avatar_badge.as_deref(), Some("pen-line"));
}

#[test]
fn test_parse_avatar_badge_empty_ignored() {
    let content = r#"---
name: test-agent
avatar_badge:
---

Prompt."#;

    let def = parse_agent_content(content, Path::new("/tmp/test-agent.md")).unwrap();
    assert!(
        def.avatar_badge.is_none(),
        "Empty avatar_badge should be None"
    );
}

#[test]
fn test_parse_unknown_fields_ignored() {
    let content = r#"---
name: test-agent
tools: Bash, Read, Write
custom_field: whatever
---

Prompt."#;

    let def = parse_agent_content(content, Path::new("/tmp/test-agent.md")).unwrap();
    assert_eq!(def.name, "test-agent");
    assert_eq!(def.system_prompt, "Prompt.");
}

#[test]
fn test_load_agent_definition_not_found() {
    let result = load_agent_definition("nonexistent-agent-xyz-12345");
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[test]
fn test_agent_definition_paths_returns_candidates() {
    let paths = agent_definition_paths("my-agent");
    assert!(
        !paths.is_empty(),
        "Should return at least one candidate path"
    );
    for path in &paths {
        assert!(
            path.ends_with("agents/my-agent.md"),
            "Path should end with agents/my-agent.md, got: {}",
            path.display()
        );
    }
}

#[test]
fn test_parse_multiline_description_uses_first_line() {
    // YAML frontmatter with a long description on one line
    let content = r#"---
name: reviewer
description: Use this agent when reviewing pull requests for quality, correctness, and style issues
model: sonnet
---

Review the code carefully."#;

    let def = parse_agent_content(content, Path::new("/tmp/reviewer.md")).unwrap();
    assert_eq!(def.name, "reviewer");
    assert!(def.description.unwrap().contains("reviewing pull requests"));
    assert_eq!(def.model.as_deref(), Some("sonnet"));
}

#[test]
fn test_body_with_horizontal_rules_preserved() {
    let content = r#"---
name: test-agent
---

First paragraph.

---

Second paragraph after horizontal rule.

---

Third paragraph."#;

    let def = parse_agent_content(content, Path::new("/tmp/test-agent.md")).unwrap();
    assert!(
        def.system_prompt.contains("Second paragraph"),
        "Body should preserve content after horizontal rules"
    );
    assert!(
        def.system_prompt.contains("Third paragraph"),
        "Body should preserve all content after multiple horizontal rules"
    );
}

#[test]
fn test_quoted_model_value_stripped() {
    let content = r#"---
name: test
model: "opus"
---

Prompt."#;

    let def = parse_agent_content(content, Path::new("/tmp/test.md")).unwrap();
    assert_eq!(
        def.model.as_deref(),
        Some("opus"),
        "Quoted model value should have quotes stripped"
    );
}

#[test]
fn test_single_quoted_values_stripped() {
    let content = r#"---
name: 'my-agent'
description: 'A test agent'
model: 'sonnet'
---

Prompt."#;

    let def = parse_agent_content(content, Path::new("/tmp/test.md")).unwrap();
    assert_eq!(def.name, "my-agent");
    assert_eq!(def.description.as_deref(), Some("A test agent"));
    assert_eq!(def.model.as_deref(), Some("sonnet"));
}
