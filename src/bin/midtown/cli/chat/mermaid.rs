//! Mermaid diagram parsing and rendering for the chat TUI
//!
//! Detects ```mermaid code fences in chat messages. Server-side rendering has
//! been removed (selkie-rs dependency dropped). The web app renders diagrams
//! client-side via mermaid-js; the TUI shows raw mermaid source.
//! Results are cached by content hash.

use std::collections::HashMap;

/// A segment of message content: either plain text, a mermaid diagram, a fenced code block,
/// or an insight block
#[derive(Debug, Clone, PartialEq)]
pub enum ContentSegment {
    /// Regular text content (may contain newlines)
    Text(String),
    /// Mermaid diagram source code
    Mermaid(String),
    /// Non-mermaid fenced code block with language tag and source
    CodeBlock { language: String, source: String },
    /// Insight content (from 💡 prefix or ★ Insight blocks)
    Insight(String),
}

/// Parse message content into segments, extracting mermaid code fences and fenced code blocks.
///
/// Detects ```mermaid ... ``` blocks and splits the content into
/// Text, Mermaid, and CodeBlock segments.
pub fn parse_content_segments(content: &str) -> Vec<ContentSegment> {
    // Format 1: whole-message insight (coworker 💡 prefix)
    let trimmed_content = content.trim_start();
    if trimmed_content.starts_with('💡') {
        let stripped = trimmed_content
            .trim_start_matches('💡')
            .trim_start()
            .to_string();
        return vec![ContentSegment::Insight(stripped)];
    }

    let mut segments = Vec::new();
    let mut current_text = String::new();
    let mut in_mermaid = false;
    let mut mermaid_source = String::new();
    // State for a non-mermaid fence: (language, buffered_source)
    let mut other_fence: Option<(String, String)> = None;
    // State for ★ Insight block
    let mut in_insight = false;
    let mut insight_source = String::new();

    for line in content.split('\n') {
        if in_mermaid {
            if line.trim_start().starts_with("```") {
                // End of mermaid fence
                in_mermaid = false;
                let trimmed = mermaid_source.trim().to_string();
                if !trimmed.is_empty() {
                    segments.push(ContentSegment::Mermaid(trimmed));
                }
                mermaid_source.clear();
            } else {
                if !mermaid_source.is_empty() {
                    mermaid_source.push('\n');
                }
                mermaid_source.push_str(line);
            }
        } else if let Some((ref lang, ref mut source_buf)) = other_fence {
            if line.trim_start().starts_with("```") {
                // End of non-mermaid code fence — emit a CodeBlock segment
                if !current_text.is_empty() {
                    segments.push(ContentSegment::Text(current_text.clone()));
                    current_text.clear();
                }
                let language = lang.clone();
                let source = source_buf.clone();
                other_fence = None;
                segments.push(ContentSegment::CodeBlock { language, source });
            } else {
                if !source_buf.is_empty() {
                    source_buf.push('\n');
                }
                source_buf.push_str(line);
            }
        } else if in_insight {
            // End marker: line of 10+ dashes with optional backtick wrapping
            let trimmed_line = line.trim();
            let inner = trimmed_line
                .strip_prefix('`')
                .and_then(|s| s.strip_suffix('`'))
                .unwrap_or(trimmed_line);
            if inner.len() >= 10 && inner.chars().all(|c| c == '─') {
                let trimmed = insight_source.trim().to_string();
                if !trimmed.is_empty() {
                    segments.push(ContentSegment::Insight(trimmed));
                }
                insight_source.clear();
                in_insight = false;
            } else {
                if !insight_source.is_empty() {
                    insight_source.push('\n');
                }
                insight_source.push_str(line);
            }
        } else {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```mermaid") {
                // Start of mermaid fence
                if !current_text.is_empty() {
                    segments.push(ContentSegment::Text(current_text.clone()));
                    current_text.clear();
                }
                in_mermaid = true;
                mermaid_source.clear();
            } else if trimmed.starts_with("```") {
                // Start of non-mermaid fence — extract the language tag
                let lang = trimmed.trim_start_matches('`').trim().to_string();
                other_fence = Some((lang, String::new()));
            } else if line.contains("★ Insight") {
                // Start of ★ Insight block
                if !current_text.is_empty() {
                    segments.push(ContentSegment::Text(current_text.clone()));
                    current_text.clear();
                }
                in_insight = true;
                insight_source.clear();
            } else {
                if !current_text.is_empty() {
                    current_text.push('\n');
                }
                current_text.push_str(line);
            }
        }
    }

    // Handle unclosed mermaid fence: treat as text
    if in_mermaid && !mermaid_source.is_empty() {
        if !current_text.is_empty() {
            current_text.push('\n');
        }
        current_text.push_str("```mermaid\n");
        current_text.push_str(&mermaid_source);
    }

    // Handle unclosed non-mermaid fence: treat as text
    if let Some((lang, source_buf)) = other_fence {
        if !current_text.is_empty() {
            current_text.push('\n');
        }
        current_text.push_str("```");
        current_text.push_str(&lang);
        if !source_buf.is_empty() {
            current_text.push('\n');
            current_text.push_str(&source_buf);
        }
    }

    // Handle unclosed insight block: emit what we have
    if in_insight && !insight_source.is_empty() {
        let trimmed = insight_source.trim().to_string();
        if !trimmed.is_empty() {
            segments.push(ContentSegment::Insight(trimmed));
        }
    }

    if !current_text.is_empty() {
        segments.push(ContentSegment::Text(current_text));
    }

    // If no segments were produced, return a single empty text segment
    if segments.is_empty() {
        segments.push(ContentSegment::Text(String::new()));
    }

    segments
}

/// A rendered mermaid diagram with ASCII art for terminal display and SVG for browser viewing
#[derive(Debug, Clone)]
pub struct RenderedDiagram {
    /// ASCII art representation for inline terminal display
    pub ascii_art: String,
    /// SVG string for browser viewing
    pub svg: String,
}

/// Compute a simple hash for mermaid content (for cache keys)
pub fn content_hash(source: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Cache for rendered mermaid diagrams
pub struct MermaidCache {
    /// Completed renders: hash -> RenderedDiagram
    diagrams: HashMap<u64, RenderedDiagram>,
    /// Hashes tracked as pending (used only in tests now that server-side
    /// rendering is removed)
    pending: HashMap<u64, ()>,
}

impl MermaidCache {
    pub fn new() -> Self {
        Self {
            diagrams: HashMap::new(),
            pending: HashMap::new(),
        }
    }

    /// Number of completed diagram renders.
    ///
    /// Used as part of the message render cache key — when a diagram finishes
    /// rendering, this count changes and invalidates the cache.
    pub fn completed_count(&self) -> usize {
        self.diagrams.len()
    }

    /// Get a cached rendered diagram, or queue it for rendering
    ///
    /// Returns Some(RenderedDiagram) if cached, None if server-side rendering
    /// is unavailable. Since selkie-rs was removed, this always returns None
    /// for uncached diagrams without spawning background threads.
    pub fn get_or_render(&mut self, mermaid_source: &str) -> Option<&RenderedDiagram> {
        let hash = content_hash(mermaid_source);

        // Return cached diagram if available (e.g. inserted via tests)
        if self.diagrams.contains_key(&hash) {
            return self.diagrams.get(&hash);
        }

        // Server-side rendering disabled — mermaid rendering now happens
        // client-side only (web app via mermaid-js, TUI shows raw source).
        // Don't spawn background threads that would always return None.
        None
    }

    /// Poll for completed renders from background threads.
    ///
    /// No-op since server-side rendering was removed, but kept for API
    /// compatibility with callers (e.g. app.rs tick loop).
    pub fn poll_completed(&mut self) {}

    /// Get a cached diagram by mermaid source (without queuing render)
    pub fn get_cached(&self, mermaid_source: &str) -> Option<&RenderedDiagram> {
        let hash = content_hash(mermaid_source);
        self.diagrams.get(&hash)
    }

    /// Check if a mermaid source is currently being rendered
    pub fn is_pending(&self, mermaid_source: &str) -> bool {
        let hash = content_hash(mermaid_source);
        self.pending.contains_key(&hash)
    }

    /// Insert a pre-rendered diagram into the cache (for testing)
    #[cfg(test)]
    pub fn insert_cached(&mut self, mermaid_source: &str, diagram: RenderedDiagram) {
        let hash = content_hash(mermaid_source);
        self.diagrams.insert(hash, diagram);
    }

    /// Mark a source as pending (for testing)
    #[cfg(test)]
    pub fn insert_pending(&mut self, mermaid_source: &str) {
        let hash = content_hash(mermaid_source);
        self.pending.insert(hash, ());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render mermaid source — always returns None since selkie-rs was removed.
    /// Kept only for test verification.
    fn render_mermaid_diagram(_source: &str) -> Option<RenderedDiagram> {
        None
    }

    #[test]
    fn test_parse_no_mermaid() {
        let segments = parse_content_segments("hello world");
        assert_eq!(segments, vec![ContentSegment::Text("hello world".into())]);
    }

    #[test]
    fn test_parse_only_mermaid() {
        let content = "```mermaid\ngraph TD\n  A-->B\n```";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::Mermaid("graph TD\n  A-->B".into())]
        );
    }

    #[test]
    fn test_parse_text_before_and_after_mermaid() {
        let content = "Here's a diagram:\n```mermaid\ngraph TD\n  A-->B\n```\nThat's it.";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![
                ContentSegment::Text("Here's a diagram:".into()),
                ContentSegment::Mermaid("graph TD\n  A-->B".into()),
                ContentSegment::Text("That's it.".into()),
            ]
        );
    }

    #[test]
    fn test_parse_multiple_mermaid_blocks() {
        let content = "First:\n```mermaid\ngraph LR\n  A-->B\n```\nSecond:\n```mermaid\ngraph TD\n  C-->D\n```";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![
                ContentSegment::Text("First:".into()),
                ContentSegment::Mermaid("graph LR\n  A-->B".into()),
                ContentSegment::Text("Second:".into()),
                ContentSegment::Mermaid("graph TD\n  C-->D".into()),
            ]
        );
    }

    #[test]
    fn test_parse_non_mermaid_fence() {
        let content = "```rust\nfn main() {}\n```";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::CodeBlock {
                language: "rust".to_string(),
                source: "fn main() {}".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_mermaid_still_works() {
        let content = "```mermaid\ngraph TD\n  A-->B\n```";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::Mermaid("graph TD\n  A-->B".into())]
        );
    }

    #[test]
    fn test_parse_plain_text_unaffected() {
        let content = "just plain text";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::Text("just plain text".to_string())]
        );
    }

    #[test]
    fn test_parse_code_fence_with_no_language() {
        let content = "```\nsome code\n```";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::CodeBlock {
                language: "".to_string(),
                source: "some code".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_code_fence_multiline() {
        let content = "```python\ndef hello():\n    print('hi')\n```";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::CodeBlock {
                language: "python".to_string(),
                source: "def hello():\n    print('hi')".to_string(),
            }]
        );
    }

    #[test]
    fn test_parse_text_before_and_after_code_block() {
        let content = "Here is some code:\n```rust\nfn main() {}\n```\nAnd that's it.";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![
                ContentSegment::Text("Here is some code:".to_string()),
                ContentSegment::CodeBlock {
                    language: "rust".to_string(),
                    source: "fn main() {}".to_string(),
                },
                ContentSegment::Text("And that's it.".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_unclosed_mermaid_fence() {
        let content = "```mermaid\ngraph TD\n  A-->B";
        let segments = parse_content_segments(content);
        // Unclosed fence is treated as text
        assert_eq!(
            segments,
            vec![ContentSegment::Text("```mermaid\ngraph TD\n  A-->B".into())]
        );
    }

    #[test]
    fn test_parse_empty_mermaid_fence() {
        let content = "```mermaid\n```";
        let segments = parse_content_segments(content);
        // Empty mermaid fence produces no Mermaid segment
        assert_eq!(segments, vec![ContentSegment::Text(String::new())]);
    }

    #[test]
    fn test_parse_empty_content() {
        let segments = parse_content_segments("");
        assert_eq!(segments, vec![ContentSegment::Text(String::new())]);
    }

    #[test]
    fn test_parse_lightbulb_insight() {
        let content = "💡 This is an insight from a coworker";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![ContentSegment::Insight(
                "This is an insight from a coworker".into()
            )]
        );
    }

    #[test]
    fn test_parse_star_insight_block() {
        let content = "Some text\n`★ Insight ─────────────────────────────────────`\nKey point here\n`─────────────────────────────────────────────────`\nMore text";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![
                ContentSegment::Text("Some text".into()),
                ContentSegment::Insight("Key point here".into()),
                ContentSegment::Text("More text".into()),
            ]
        );
    }

    #[test]
    fn test_parse_star_insight_no_backticks() {
        let content = "Before\n★ Insight ─────────────────────────────────────\nInsight content\n─────────────────────────────────────────────────\nAfter";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![
                ContentSegment::Text("Before".into()),
                ContentSegment::Insight("Insight content".into()),
                ContentSegment::Text("After".into()),
            ]
        );
    }

    #[test]
    fn test_parse_insight_unclosed() {
        let content = "Text\n★ Insight ─────\nUnclosed insight content";
        let segments = parse_content_segments(content);
        assert_eq!(
            segments,
            vec![
                ContentSegment::Text("Text".into()),
                ContentSegment::Insight("Unclosed insight content".into()),
            ]
        );
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("graph TD\n  A-->B");
        let h2 = content_hash("graph TD\n  A-->B");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_different() {
        let h1 = content_hash("graph TD\n  A-->B");
        let h2 = content_hash("graph LR\n  A-->B");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_mermaid_cache_new() {
        let cache = MermaidCache::new();
        assert!(cache.diagrams.is_empty());
        assert!(cache.pending.is_empty());
    }

    #[test]
    fn test_render_mermaid_diagram_returns_none() {
        // selkie-rs removed — render_mermaid_diagram always returns None
        let result = render_mermaid_diagram("graph TD\n  A-->B");
        assert!(result.is_none());
    }
}
