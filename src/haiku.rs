//! Haiku LLM scoring for PR issue prioritization.
//!
//! This module provides functions to score PR issues using Claude Haiku,
//! filtering out low-priority issues before escalation.

use serde::{Deserialize, Serialize};
use std::env;
use tracing::{debug, warn};

use crate::daemon::PrIssueType;

/// Minimum score threshold for issues to be actioned (0-100).
pub const SCORE_THRESHOLD: u8 = 80;

/// Anthropic API base URL.
const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Haiku model identifier.
const HAIKU_MODEL: &str = "claude-3-5-haiku-latest";

/// A scored PR issue with its priority score.
#[derive(Debug, Clone)]
pub struct ScoredIssue {
    pub issue_type: PrIssueType,
    pub score: u8,
    pub reasoning: String,
}

impl ScoredIssue {
    /// Check if this issue passes the score threshold.
    pub fn passes_threshold(&self) -> bool {
        self.score >= SCORE_THRESHOLD
    }
}

/// Request body for Anthropic messages API.
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

/// Response from Anthropic messages API.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    text: String,
}

/// Score a PR issue using Claude Haiku.
///
/// Returns a score from 0-100 indicating priority:
/// - 0-39: Low priority, probably noise
/// - 40-79: Medium priority, monitor but don't escalate
/// - 80-100: High priority, requires immediate action
pub async fn score_issue(
    pr_number: u64,
    pr_title: &str,
    issue_type: PrIssueType,
    pr_context: &serde_json::Value,
) -> Option<ScoredIssue> {
    let api_key = match env::var("ANTHROPIC_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            debug!("ANTHROPIC_API_KEY not set, skipping issue scoring");
            // Return issue with max score when API key not available
            // This ensures issues are processed without scoring
            return Some(ScoredIssue {
                issue_type,
                score: 100,
                reasoning: "Scoring skipped (no API key)".to_string(),
            });
        }
    };

    let prompt = build_scoring_prompt(pr_number, pr_title, issue_type, pr_context);

    match call_haiku(&api_key, &prompt).await {
        Ok(response) => parse_score_response(&response, issue_type),
        Err(e) => {
            warn!("Failed to score issue via Haiku: {}", e);
            // Return issue with max score on API failure to ensure processing
            Some(ScoredIssue {
                issue_type,
                score: 100,
                reasoning: format!("Scoring failed: {}", e),
            })
        }
    }
}

/// Score multiple issues in parallel.
pub async fn score_issues(
    pr_number: u64,
    pr_title: &str,
    issues: Vec<PrIssueType>,
    pr_context: &serde_json::Value,
) -> Vec<ScoredIssue> {
    let pr_context = pr_context.clone();
    let futures: Vec<_> = issues
        .into_iter()
        .map(|issue_type| {
            let ctx = pr_context.clone();
            async move { score_issue(pr_number, pr_title, issue_type, &ctx).await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    results.into_iter().flatten().collect()
}

/// Filter issues to only those passing the score threshold.
pub fn filter_by_threshold(scored_issues: Vec<ScoredIssue>) -> Vec<ScoredIssue> {
    scored_issues
        .into_iter()
        .filter(|issue| issue.passes_threshold())
        .collect()
}

/// Build the scoring prompt for a PR issue.
fn build_scoring_prompt(
    pr_number: u64,
    pr_title: &str,
    issue_type: PrIssueType,
    pr_context: &serde_json::Value,
) -> String {
    let context_summary = summarize_pr_context(pr_context, issue_type);

    format!(
        r#"You are scoring the priority of a PR issue to determine if it needs immediate attention.

PR #{}: "{}"
Issue Type: {}

Context:
{}

Score this issue from 0-100 based on urgency and impact:
- 0-39: Low priority (noise, transient, or self-resolving)
- 40-79: Medium priority (should be addressed but not urgent)
- 80-100: High priority (requires immediate attention)

Consider:
1. Is this a blocking issue or just informational?
2. How long has this state persisted?
3. Is human intervention actually needed?
4. Could this resolve on its own (e.g., temporary CI flakiness)?

Respond with ONLY a JSON object in this format:
{{"score": <number>, "reasoning": "<brief explanation>"}}
"#,
        pr_number, pr_title, issue_type, context_summary
    )
}

/// Summarize PR context relevant to the issue type.
fn summarize_pr_context(pr: &serde_json::Value, issue_type: PrIssueType) -> String {
    let mut summary = String::new();

    match issue_type {
        PrIssueType::MergeConflict => {
            let mergeable = pr
                .get("mergeable")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            summary.push_str(&format!("Mergeable status: {}\n", mergeable));
        }
        PrIssueType::CiFailed => {
            if let Some(checks) = pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
                summary.push_str("CI Check Results:\n");
                for check in checks {
                    let name = check
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let conclusion = check
                        .get("conclusion")
                        .and_then(|c| c.as_str())
                        .unwrap_or("pending");
                    summary.push_str(&format!("  - {}: {}\n", name, conclusion));
                }
            }
        }
        PrIssueType::ChangesRequested | PrIssueType::Approved => {
            let decision = pr
                .get("reviewDecision")
                .and_then(|r| r.as_str())
                .unwrap_or("none");
            summary.push_str(&format!("Review decision: {}\n", decision));
        }
        PrIssueType::NeedsReview | PrIssueType::ReviewComment => {
            let decision = pr
                .get("reviewDecision")
                .and_then(|r| r.as_str())
                .unwrap_or("none");
            let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
            summary.push_str(&format!("Review decision: {}\n", decision));
            summary.push_str(&format!("Is draft: {}\n", is_draft));
        }
    }

    // Add creation time if available
    if let Some(created) = pr.get("createdAt").and_then(|c| c.as_str()) {
        summary.push_str(&format!("Created: {}\n", created));
    }

    summary
}

/// Call Haiku API with a prompt.
async fn call_haiku(api_key: &str, prompt: &str) -> Result<String, String> {
    let client = reqwest::Client::new();

    let request = AnthropicRequest {
        model: HAIKU_MODEL.to_string(),
        max_tokens: 150,
        messages: vec![AnthropicMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
        }],
    };

    let response = client
        .post(ANTHROPIC_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, body));
    }

    let api_response: AnthropicResponse = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    api_response
        .content
        .first()
        .map(|block| block.text.clone())
        .ok_or_else(|| "Empty response from API".to_string())
}

/// Parse the score response from Haiku.
fn parse_score_response(response: &str, issue_type: PrIssueType) -> Option<ScoredIssue> {
    // Try to parse as JSON
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(response) {
        let score = parsed.get("score").and_then(|s| s.as_u64()).unwrap_or(100) as u8;
        let reasoning = parsed
            .get("reasoning")
            .and_then(|r| r.as_str())
            .unwrap_or("No reasoning provided")
            .to_string();

        return Some(ScoredIssue {
            issue_type,
            score,
            reasoning,
        });
    }

    // Fallback: try to extract a number from the response
    let score = response
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(3)
        .collect::<String>()
        .parse::<u8>()
        .unwrap_or(100);

    Some(ScoredIssue {
        issue_type,
        score,
        reasoning: "Could not parse structured response".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_threshold() {
        let high_score = ScoredIssue {
            issue_type: PrIssueType::CiFailed,
            score: 85,
            reasoning: "Critical failure".to_string(),
        };
        assert!(high_score.passes_threshold());

        let low_score = ScoredIssue {
            issue_type: PrIssueType::CiFailed,
            score: 50,
            reasoning: "Flaky test".to_string(),
        };
        assert!(!low_score.passes_threshold());

        let boundary_score = ScoredIssue {
            issue_type: PrIssueType::MergeConflict,
            score: 80,
            reasoning: "At threshold".to_string(),
        };
        assert!(boundary_score.passes_threshold());
    }

    #[test]
    fn test_filter_by_threshold() {
        let issues = vec![
            ScoredIssue {
                issue_type: PrIssueType::CiFailed,
                score: 90,
                reasoning: "High".to_string(),
            },
            ScoredIssue {
                issue_type: PrIssueType::MergeConflict,
                score: 60,
                reasoning: "Medium".to_string(),
            },
            ScoredIssue {
                issue_type: PrIssueType::Approved,
                score: 85,
                reasoning: "High".to_string(),
            },
        ];

        let filtered = filter_by_threshold(issues);
        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().all(|i| i.score >= 80));
    }

    #[test]
    fn test_parse_score_response_json() {
        let response = r#"{"score": 75, "reasoning": "Test failure looks flaky"}"#;
        let result = parse_score_response(response, PrIssueType::CiFailed).unwrap();
        assert_eq!(result.score, 75);
        assert_eq!(result.reasoning, "Test failure looks flaky");
    }

    #[test]
    fn test_parse_score_response_fallback() {
        let response = "The score is 85 because this is critical";
        let result = parse_score_response(response, PrIssueType::CiFailed).unwrap();
        assert_eq!(result.score, 85);
    }

    #[test]
    fn test_build_scoring_prompt() {
        let pr = serde_json::json!({
            "mergeable": "CONFLICTING",
            "createdAt": "2024-01-15T10:00:00Z"
        });
        let prompt = build_scoring_prompt(42, "Fix auth bug", PrIssueType::MergeConflict, &pr);
        assert!(prompt.contains("PR #42"));
        assert!(prompt.contains("Fix auth bug"));
        assert!(prompt.contains("merge conflict"));
        assert!(prompt.contains("CONFLICTING"));
    }
}
