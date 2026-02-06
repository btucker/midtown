//! Mermaid diagram parsing and rendering for the chat TUI
//!
//! Detects ```mermaid code fences in chat messages, renders them to PNG
//! using selkie-rs (a pure Rust mermaid implementation), and caches results
//! by content hash.

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

/// A rendered mermaid image with its dimensions
#[derive(Debug, Clone)]
pub struct RenderedImage {
    /// PNG image data
    pub png_data: Vec<u8>,
    /// Image width in pixels (used for aspect ratio in tests)
    #[allow(dead_code)]
    pub width: u32,
    /// Image height in pixels (used for aspect ratio in tests)
    #[allow(dead_code)]
    pub height: u32,
}

/// Compute a simple hash for mermaid content (for cache keys)
fn content_hash(source: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

/// Cache for rendered mermaid diagrams
pub struct MermaidCache {
    /// Completed renders: hash -> RenderedImage
    images: HashMap<u64, RenderedImage>,
    /// Hashes currently being rendered (to avoid duplicate work)
    pending: HashMap<u64, ()>,
    /// Receiver for completed renders from background threads
    receiver: Option<Receiver<(u64, Option<RenderedImage>)>>,
    /// Sender for queueing render requests
    sender: Option<std::sync::mpsc::Sender<(u64, Option<RenderedImage>)>>,
}

impl MermaidCache {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            pending: HashMap::new(),
            receiver: None,
            sender: None,
        }
    }

    /// Get a cached rendered image, or queue it for rendering
    ///
    /// Returns Some(RenderedImage) if cached, None if not yet rendered.
    /// Automatically queues un-cached diagrams for background rendering.
    pub fn get_or_render(&mut self, mermaid_source: &str) -> Option<&RenderedImage> {
        let hash = content_hash(mermaid_source);

        // Already cached
        if self.images.contains_key(&hash) {
            return self.images.get(&hash);
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
            let result = render_mermaid_to_png(&source);
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
                    Ok((hash, Some(image))) => {
                        self.pending.remove(&hash);
                        self.images.insert(hash, image);
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

    /// Get a cached image by mermaid source (without queuing render)
    pub fn get_cached(&self, mermaid_source: &str) -> Option<&RenderedImage> {
        let hash = content_hash(mermaid_source);
        self.images.get(&hash)
    }

    /// Check if a mermaid source is currently being rendered
    pub fn is_pending(&self, mermaid_source: &str) -> bool {
        let hash = content_hash(mermaid_source);
        self.pending.contains_key(&hash)
    }

    /// Insert a pre-rendered image into the cache (for testing)
    #[cfg(test)]
    pub fn insert_cached(&mut self, mermaid_source: &str, image: RenderedImage) {
        let hash = content_hash(mermaid_source);
        self.images.insert(hash, image);
    }

    /// Mark a source as pending (for testing)
    #[cfg(test)]
    pub fn insert_pending(&mut self, mermaid_source: &str) {
        let hash = content_hash(mermaid_source);
        self.pending.insert(hash, ());
    }
}

/// Render mermaid source to PNG using selkie-rs (pure Rust, no external process)
///
/// Returns None if rendering fails (invalid syntax, etc.)
fn render_mermaid_to_png(source: &str) -> Option<RenderedImage> {
    // Use selkie-rs to render mermaid source to SVG with dark theme
    let svg = selkie::render::render_text(&format!(
        "%%{{init: {{\"theme\": \"dark\"}}}}%%\n{}",
        source
    ))
    .ok()?;

    // Convert SVG to PNG using resvg
    svg_to_png(&svg, 800)
}

/// Convert SVG string to PNG image data at a given width
fn svg_to_png(svg: &str, target_width: u32) -> Option<RenderedImage> {
    use resvg::{tiny_skia, usvg};

    let mut opt = usvg::Options::default();
    let fontdb = opt.fontdb_mut();
    fontdb.load_system_fonts();

    let tree = usvg::Tree::from_str(svg, &opt).ok()?;

    let svg_size = tree.size();
    let scale = target_width as f32 / svg_size.width();
    let target_height = (svg_size.height() * scale) as u32;

    let mut pixmap = tiny_skia::Pixmap::new(target_width, target_height)?;
    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let png_data = pixmap.encode_png().ok()?;

    Some(RenderedImage {
        png_data,
        width: target_width,
        height: target_height,
    })
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
        assert!(cache.images.is_empty());
        assert!(cache.pending.is_empty());
    }

    #[test]
    fn test_render_mermaid_to_png_simple_flowchart() {
        let result = render_mermaid_to_png("graph TD\n  A-->B");
        assert!(result.is_some(), "Simple flowchart should render");
        let image = result.unwrap();
        assert!(!image.png_data.is_empty(), "PNG data should not be empty");
        assert!(image.width > 0, "Width should be positive");
        assert!(image.height > 0, "Height should be positive");
        // Verify it's a valid PNG (check signature)
        assert_eq!(&image.png_data[0..4], b"\x89PNG");
    }

    #[test]
    fn test_render_mermaid_to_png_sequence_diagram() {
        let source = "sequenceDiagram\n    Alice->>Bob: Hello\n    Bob->>Alice: Hi";
        let result = render_mermaid_to_png(source);
        assert!(result.is_some(), "Sequence diagram should render");
        let image = result.unwrap();
        assert!(!image.png_data.is_empty(), "PNG data should not be empty");
        assert!(image.width > 0, "Width should be positive");
        assert!(image.height > 0, "Height should be positive");
        assert_eq!(&image.png_data[0..4], b"\x89PNG");
    }

    #[test]
    fn test_render_mermaid_to_png_invalid_input() {
        let result = render_mermaid_to_png("this is not valid mermaid syntax }{}{}{");
        // Invalid input should return None (selkie-rs parse failure)
        assert!(result.is_none());
    }
}
