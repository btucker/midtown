use super::*;

#[test]
fn test_extract_coworker_from_pr_body() {
    assert_eq!(
        extract_coworker_from_pr_body("<!-- midtown: york -->\n## Summary"),
        Some("york".to_string())
    );
    assert_eq!(
        extract_coworker_from_pr_body("<!--midtown:  park  -->\nDesc"),
        Some("park".to_string())
    );
    assert_eq!(extract_coworker_from_pr_body("no frontmatter here"), None);
    assert_eq!(extract_coworker_from_pr_body(""), None);
}

#[test]
fn test_extract_reviewer_from_pr_comments() {
    let comments = vec![serde_json::json!({
        "body": "<!-- midtown: lexington -->\n\n### Code review\nNo issues.",
        "createdAt": "2026-01-29T10:00:00Z"
    })];
    let (reviewer, at) = extract_reviewer_from_pr_comments(&comments);
    assert_eq!(reviewer, Some("lexington".to_string()));
    assert_eq!(at, Some("2026-01-29T10:00:00Z".to_string()));

    let comments = vec![serde_json::json!({
        "body": "## Code Review by vernon\nLGTM",
        "createdAt": "2026-01-29T11:00:00Z"
    })];
    let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
    assert_eq!(reviewer, Some("vernon".to_string()));

    let (reviewer, _) = extract_reviewer_from_pr_comments(&[]);
    assert_eq!(reviewer, None);
}

#[test]
fn test_pr_ci_status() {
    assert_eq!(pr_ci_status(&[]), "unknown");
    assert_eq!(
        pr_ci_status(&[serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})]),
        "passed"
    );
    assert_eq!(
        pr_ci_status(&[serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})]),
        "failed"
    );
    assert_eq!(
        pr_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
        "running"
    );
}

#[test]
fn test_prs_cache_hit_and_miss() {
    let cache = PrsCache::new();
    let key: u64 = 42;
    let value = serde_json::json!({"prs": []});

    assert!(cache.get(key).is_none(), "empty cache should miss");
    cache.set(value.clone(), key);
    assert_eq!(cache.get(key), Some(value), "should hit after set");
    assert!(cache.get(key + 1).is_none(), "different key should miss");
}
