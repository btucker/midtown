//! Mermaid diagram parsing and rendering for the chat TUI
//!
//! Detects ```mermaid code fences in chat messages, renders them to PNG
//! using the mmdc CLI, and caches results by content hash.

use std::collections::HashMap;
use std::process::Command;
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
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
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

/// Read PNG dimensions from the IHDR chunk
fn read_png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    // PNG signature (8 bytes) + IHDR length (4 bytes) + "IHDR" (4 bytes) + width (4 bytes) + height (4 bytes)
    if data.len() < 24 {
        return None;
    }
    // Check PNG signature
    if &data[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    // Width and height are at bytes 16-23 (big-endian u32)
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Some((width, height))
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
    /// Whether mmdc is available on the system
    mmdc_available: Option<bool>,
}

impl MermaidCache {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            pending: HashMap::new(),
            receiver: None,
            sender: None,
            mmdc_available: None,
        }
    }

    /// Check if mmdc CLI is available
    fn is_mmdc_available(&mut self) -> bool {
        if let Some(available) = self.mmdc_available {
            return available;
        }
        let available = Command::new("mmdc").arg("--version").output().is_ok();
        self.mmdc_available = Some(available);
        available
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

        // Check if mmdc is available
        if !self.is_mmdc_available() {
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
}

/// Render mermaid source to PNG using the mmdc CLI
///
/// Returns None if rendering fails (mmdc not found, invalid syntax, etc.)
fn render_mermaid_to_png(source: &str) -> Option<RenderedImage> {
    let temp_dir = std::env::temp_dir();
    let id = uuid::Uuid::new_v4();
    let input_path = temp_dir.join(format!("midtown-mermaid-{}.mmd", id));
    let output_path = temp_dir.join(format!("midtown-mermaid-{}.png", id));

    // Write mermaid source to temp file
    if std::fs::write(&input_path, source).is_err() {
        return None;
    }

    // Run mmdc to render
    let result = Command::new("mmdc")
        .args([
            "-i",
            input_path.to_str()?,
            "-o",
            output_path.to_str()?,
            "-w",
            "800",
            "-t",
            "dark",
            "-b",
            "transparent",
            "--quiet",
        ])
        .output();

    // Clean up input file
    let _ = std::fs::remove_file(&input_path);

    match result {
        Ok(output) if output.status.success() => {
            // Read the PNG output
            let png_data = std::fs::read(&output_path).ok()?;
            let _ = std::fs::remove_file(&output_path);

            let (width, height) = read_png_dimensions(&png_data)?;

            Some(RenderedImage {
                png_data,
                width,
                height,
            })
        }
        _ => {
            let _ = std::fs::remove_file(&output_path);
            None
        }
    }
}

/// Estimate the number of terminal rows an image will occupy
///
/// Uses an assumed cell aspect ratio of ~2:1 (cells are taller than wide).
/// A typical terminal cell is about 8px wide and 16px tall.
pub fn estimate_image_rows(image: &RenderedImage, available_cols: u16) -> u16 {
    if image.width == 0 || image.height == 0 {
        return 1;
    }

    // Estimate rows needed when the image is scaled to fill available_cols.
    // Terminal cells are typically ~2:1 aspect ratio (8px wide, 16px tall),
    // so divide the pixel-level height ratio by 2 to get row count.
    let rows = (image.height as f64 * available_cols as f64 / image.width as f64 / 2.0).ceil();

    // Clamp to reasonable bounds
    (rows as u16).clamp(1, 20)
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
    fn test_read_png_dimensions() {
        // Minimal valid PNG header with 100x50 dimensions
        let data = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
            0x00, 0x00, 0x00, 0x0D, // IHDR length
            b'I', b'H', b'D', b'R', // IHDR type
            0x00, 0x00, 0x00, 0x64, // width = 100
            0x00, 0x00, 0x00, 0x32, // height = 50
        ];
        assert_eq!(read_png_dimensions(&data), Some((100, 50)));
    }

    #[test]
    fn test_read_png_dimensions_too_short() {
        assert_eq!(read_png_dimensions(&[0; 10]), None);
    }

    #[test]
    fn test_read_png_dimensions_bad_signature() {
        let data = vec![0u8; 24];
        assert_eq!(read_png_dimensions(&data), None);
    }

    #[test]
    fn test_estimate_image_rows() {
        // 800x400 image at 80 cols -> aspect ratio 2:1
        let image = RenderedImage {
            png_data: vec![],
            width: 800,
            height: 400,
        };
        let rows = estimate_image_rows(&image, 80);
        assert!(
            (10..=20).contains(&rows),
            "Expected 10-20 rows, got {}",
            rows
        );
    }

    #[test]
    fn test_estimate_image_rows_clamped() {
        // Very tall image should be capped at 20
        let image = RenderedImage {
            png_data: vec![],
            width: 100,
            height: 10000,
        };
        let rows = estimate_image_rows(&image, 80);
        assert_eq!(rows, 20);
    }

    #[test]
    fn test_mermaid_cache_new() {
        let cache = MermaidCache::new();
        assert!(cache.images.is_empty());
        assert!(cache.pending.is_empty());
    }
}
