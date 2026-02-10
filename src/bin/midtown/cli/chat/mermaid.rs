//! Mermaid diagram parsing and rendering for the chat TUI
//!
//! Detects ```mermaid code fences in chat messages, renders them to ASCII art
//! (for inline terminal display) and SVG (for browser viewing) using selkie-rs.
//! Results are cached by content hash.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

/// A segment of message content: either plain text or a mermaid diagram
#[derive(Debug, Clone, PartialEq)]
pub enum ContentSegment {
    /// Regular text content (may contain newlines)
    Text(String),
    /// Mermaid diagram source code
    Mermaid(String),
}

/// Parse message content into segments, extracting mermaid code fences.
///
/// Detects ```mermaid ... ``` blocks and splits the content into
/// Text and Mermaid segments.
pub fn parse_content_segments(content: &str) -> Vec<ContentSegment> {
    let mut segments = Vec::new();
    let mut current_text = String::new();
    let mut in_mermaid = false;
    let mut mermaid_source = String::new();
    let mut in_other_fence = false;

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
        } else if in_other_fence {
            if line.trim_start().starts_with("```") {
                in_other_fence = false;
            }
            // Pass through non-mermaid fenced content as text
            if !current_text.is_empty() {
                current_text.push('\n');
            }
            current_text.push_str(line);
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
                // Start of non-mermaid fence - pass through as text
                in_other_fence = true;
                if !current_text.is_empty() {
                    current_text.push('\n');
                }
                current_text.push_str(line);
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
    /// Hashes currently being rendered (to avoid duplicate work)
    pending: HashMap<u64, ()>,
    /// Receiver for completed renders from background threads
    receiver: Option<Receiver<(u64, Option<RenderedDiagram>)>>,
    /// Sender for queueing render requests
    sender: Option<std::sync::mpsc::Sender<(u64, Option<RenderedDiagram>)>>,
}

impl MermaidCache {
    pub fn new() -> Self {
        Self {
            diagrams: HashMap::new(),
            pending: HashMap::new(),
            receiver: None,
            sender: None,
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
    /// Returns Some(RenderedDiagram) if cached, None if not yet rendered.
    /// Automatically queues un-cached diagrams for background rendering.
    pub fn get_or_render(&mut self, mermaid_source: &str) -> Option<&RenderedDiagram> {
        let hash = content_hash(mermaid_source);

        // Already cached
        if self.diagrams.contains_key(&hash) {
            return self.diagrams.get(&hash);
        }

        // Already pending render
        if self.pending.contains_key(&hash) {
            return None;
        }

        // Queue for background rendering
        self.pending.insert(hash, ());

        let (tx, rx) = if self.sender.is_some() {
            // Reuse existing channel
            (self.sender.clone().unwrap(), None)
        } else {
            let (tx, rx) = mpsc::channel();
            self.sender = Some(tx.clone());
            (tx, Some(rx))
        };

        if let Some(rx) = rx {
            self.receiver = Some(rx);
        }

        let source = mermaid_source.to_string();
        thread::spawn(move || {
            let result = render_mermaid_diagram(&source);
            let _ = tx.send((hash, result));
        });

        None
    }

    /// Poll for completed renders from background threads
    pub fn poll_completed(&mut self) {
        if let Some(ref receiver) = self.receiver {
            // Drain all available results
            loop {
                match receiver.try_recv() {
                    Ok((hash, Some(diagram))) => {
                        self.pending.remove(&hash);
                        self.diagrams.insert(hash, diagram);
                    }
                    Ok((hash, None)) => {
                        // Render failed - remove from pending so we don't retry forever
                        self.pending.remove(&hash);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.receiver = None;
                        self.sender = None;
                        break;
                    }
                }
            }
        }
    }

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

/// Render mermaid source to ASCII art + SVG using selkie-rs
///
/// Returns None if rendering fails (invalid syntax, etc.)
fn render_mermaid_diagram(source: &str) -> Option<RenderedDiagram> {
    let dark_source = format!("%%{{init: {{\"theme\": \"dark\"}}}}%%\n{}", source);

    // Render SVG (works for all diagram types)
    let svg = selkie::render::render_text(&dark_source).ok()?;

    // Render ASCII art (selkie v0.3 supports all diagram types)
    let ascii_art =
        selkie::render::render_text_ascii(source).unwrap_or_else(|_| source.to_string());

    Some(RenderedDiagram { ascii_art, svg })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            vec![ContentSegment::Text("```rust\nfn main() {}\n```".into())]
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
    fn test_render_mermaid_diagram_simple_flowchart() {
        let result = render_mermaid_diagram("graph TD\n  A-->B");
        assert!(result.is_some(), "Simple flowchart should render");
        let diagram = result.unwrap();
        assert!(!diagram.svg.is_empty(), "SVG should not be empty");
        assert!(diagram.svg.contains("<svg"), "SVG should contain <svg tag");
        assert!(
            !diagram.ascii_art.is_empty(),
            "ASCII art should not be empty"
        );
    }

    #[test]
    fn test_render_mermaid_diagram_sequence() {
        let source = "sequenceDiagram\n    Alice->>Bob: Hello\n    Bob->>Alice: Hi";
        let result = render_mermaid_diagram(source);
        assert!(result.is_some(), "Sequence diagram should render");
        let diagram = result.unwrap();
        assert!(!diagram.svg.is_empty(), "SVG should not be empty");
        assert!(diagram.svg.contains("<svg"), "SVG should contain <svg tag");
        assert!(
            diagram.ascii_art.contains("Alice"),
            "ASCII art should contain participant names"
        );
    }

    #[test]
    fn test_render_mermaid_diagram_invalid_input() {
        let result = render_mermaid_diagram("this is not valid mermaid syntax }{}{}{");
        // Invalid input should return None (selkie-rs parse failure)
        assert!(result.is_none());
    }

    #[test]
    fn test_render_mermaid_diagram_flowchart_ascii() {
        let result = render_mermaid_diagram("graph TD\n  A[Hello]-->B[World]");
        assert!(result.is_some(), "Flowchart should render");
        let diagram = result.unwrap();
        assert!(
            diagram.ascii_art.contains("Hello"),
            "ASCII art should contain node labels"
        );
    }

    #[test]
    fn test_render_mermaid_diagram_sequence_ascii() {
        let result = render_mermaid_diagram("sequenceDiagram\n    Alice->>Bob: Hello");
        assert!(result.is_some(), "Sequence diagram should render");
        let diagram = result.unwrap();
        assert!(
            diagram.ascii_art.contains("Alice"),
            "ASCII art should contain participant names"
        );
    }
}
