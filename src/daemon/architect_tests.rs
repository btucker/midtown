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
    let role = ArchitectRole;
    let prompt = role.system_prompt();
    assert!(
        prompt.contains("midtown diagram validate"),
        "system prompt should include midtown diagram validate command"
    );
    assert!(
        prompt.contains("2 fix attempts"),
        "system prompt should cap retries at 2"
    );
}
