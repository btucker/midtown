//! Structured frontmatter parsing for GitHub PR/issue comments.
//!
//! Midtown coworkers embed metadata in HTML comments using the format
//! `<!-- midtown [key:value ...] -->`. This module parses and generates
//! those tags for review attribution, placeholder tracking, and session
//! identification.

use tracing::warn;

// ---------------------------------------------------------------------------
// Structured frontmatter: `<!-- midtown [key:value ...] -->`
// ---------------------------------------------------------------------------

/// The type of a midtown comment, encoded as `type:X` in frontmatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentType {
    ReviewPlaceholder,
    Review,
}

/// Parsed fields from a `<!-- midtown ... -->` frontmatter tag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MidtownFrontmatter {
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub comment_type: Option<CommentType>,
}

impl MidtownFrontmatter {
    pub fn is_placeholder(&self) -> bool {
        self.comment_type == Some(CommentType::ReviewPlaceholder)
    }

    pub fn is_review(&self) -> bool {
        self.comment_type == Some(CommentType::Review)
    }
}

/// Parse structured frontmatter from a comment body.
///
/// Matches `<!-- midtown [key:value ...] -->` where keys are:
/// - `session:{id}` — Claude Code session ID
/// - `task:{id}` — task ID
/// - `type:review-placeholder` or `type:review`
///
/// Returns `None` if no `<!-- midtown` tag is found.
pub fn parse_frontmatter(body: &str) -> Option<MidtownFrontmatter> {
    let start = body.find("<!-- midtown")?;
    let after_start = &body[start + 12..]; // len("<!-- midtown")
    let end = after_start.find("-->")?;
    let content = after_start[..end].trim();

    let mut fm = MidtownFrontmatter::default();
    for token in content.split_whitespace() {
        if let Some(val) = token.strip_prefix("session:") {
            if !val.is_empty() {
                fm.session_id = Some(val.to_string());
            }
        } else if let Some(val) = token.strip_prefix("task:") {
            if !val.is_empty() {
                fm.task_id = Some(val.to_string());
            }
        } else if let Some(val) = token.strip_prefix("type:") {
            fm.comment_type = match val {
                "review-placeholder" => Some(CommentType::ReviewPlaceholder),
                "review" => Some(CommentType::Review),
                _ => None,
            };
        }
    }

    Some(fm)
}

/// Format a placeholder frontmatter tag.
pub fn format_placeholder_frontmatter(task_id: &str) -> String {
    format!("<!-- midtown task:{task_id} type:review-placeholder -->")
}

/// Format a completed review frontmatter tag.
pub fn format_review_frontmatter(session_id: &str, task_id: &str) -> String {
    format!("<!-- midtown session:{session_id} task:{task_id} type:review -->")
}

/// Format a general coworker frontmatter tag (non-review comments).
pub fn format_session_frontmatter(session_id: &str) -> String {
    format!("<!-- midtown session:{session_id} -->")
}

/// Check if text contains a coworker review signature.
///
/// Returns true only if the text contains structured frontmatter with `type:review`
/// (i.e., `<!-- midtown ... type:review -->`). Text-based patterns are not matched.
pub fn text_contains_review_signature(text: &str) -> bool {
    if let Some(fm) = parse_frontmatter(text) {
        return fm.is_review();
    }
    false
}

/// Extract the review session ID from a review body's frontmatter.
///
/// Returns the `session:{id}` value from `<!-- midtown session:X task:Y type:review -->`.
pub fn extract_review_session_id(text: &str) -> Option<String> {
    let fm = parse_frontmatter(text)?;
    if fm.is_review() { fm.session_id } else { None }
}

/// Extract the review author name from a review body.
///
/// Checks multiple patterns (in priority order):
/// 1. Structured frontmatter `session:{id}` with `type:review` — returns session ID
///    (caller resolves to coworker name via daemon state)
/// 2. `<!-- midtown: name -->` legacy frontmatter
/// 3. `Reviewed by NAME` or `🤖 Reviewed by NAME` signature
/// 4. `## Code Review by NAME` header pattern
///
/// Returns `None` if no author can be determined.
pub fn extract_review_author_from_body(text: &str) -> Option<String> {
    // Priority 1: new structured frontmatter (session ID as author identifier)
    if let Some(session_id) = extract_review_session_id(text) {
        return Some(session_id);
    }

    // Priority 2: legacy frontmatter (explicit name)
    if let Some(name) = extract_midtown_frontmatter_name(text) {
        return Some(name);
    }

    // Priority 3: "Reviewed by NAME" or "🤖 Reviewed by NAME"
    for line in text.lines() {
        for prefix in &["🤖 Reviewed by ", "Reviewed by "] {
            if let Some(rest) = line.find(prefix).map(|i| &line[i + prefix.len()..]) {
                let name = rest.split_whitespace().next().unwrap_or("").trim();
                if !name.is_empty() {
                    return Some(name.to_lowercase());
                }
            }
        }
    }

    // Priority 4: "## Code Review by NAME" header
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let content = trimmed.trim_start_matches('#').trim();
            let content_lower = content.to_lowercase();
            if let Some(rest) = content_lower.strip_prefix("code review by ") {
                let name = rest.split_whitespace().next().unwrap_or("").trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }

    None
}

/// Extract the coworker name from `<!-- midtown: name -->` legacy frontmatter.
///
/// Unlike `coworker_from_frontmatter` (which validates against the known
/// coworker list), this returns the raw name for flexible matching.
fn extract_midtown_frontmatter_name(body: &str) -> Option<String> {
    let start = body.find("<!-- midtown:")?;
    let after_start = &body[start + 13..];
    // Skip structured frontmatter (has space-separated key:value pairs)
    // Legacy format: `<!-- midtown: name -->` (colon followed by a simple name)
    let end = after_start.find("-->")?;
    let content = after_start[..end].trim();
    // If content has key:value tokens (session:, task:, type:), it's structured — not legacy
    if content
        .split_whitespace()
        .any(|t| t.starts_with("session:") || t.starts_with("task:") || t.starts_with("type:"))
    {
        return None;
    }
    if content.is_empty() {
        None
    } else {
        Some(content.to_lowercase())
    }
}

/// Check if a review's author matches the assigned reviewer.
///
/// Comparison is case-insensitive. Returns `true` if:
/// - No assigned reviewer (accept any review)
/// - Author extracted from body matches the assigned reviewer
///
/// Returns `false` if an assigned reviewer exists but the author cannot be
/// extracted or doesn't match.
pub fn review_author_matches(
    body: &str,
    assigned_reviewer: Option<&str>,
    assigned_session_id: Option<&str>,
) -> bool {
    let Some(reviewer) = assigned_reviewer else {
        return true; // No assigned reviewer — accept any review
    };

    match extract_review_author_from_body(body) {
        Some(author) if author.starts_with('$') => {
            warn!(
                "review_author_matches: extracted author is a literal env var '{}' — treating as no match",
                author
            );
            false
        }
        Some(author) => {
            // Match by name (legacy) or by session ID (new format)
            author.eq_ignore_ascii_case(reviewer)
                || assigned_session_id.is_some_and(|sid| sid == author)
        }
        None => false,
    }
}
