use super::*;

#[test]
fn test_unescape_shell_artifacts_exclamation() {
    assert_eq!(
        unescape_shell_artifacts("Game time\\! Let's go"),
        "Game time! Let's go"
    );
}

#[test]
fn test_unescape_shell_artifacts_multiple_exclamations() {
    assert_eq!(
        unescape_shell_artifacts("Wow\\! Amazing\\! Done\\!"),
        "Wow! Amazing! Done!"
    );
}

#[test]
fn test_unescape_shell_artifacts_no_escapes() {
    assert_eq!(
        unescape_shell_artifacts("Normal message with ! marks"),
        "Normal message with ! marks"
    );
}

#[test]
fn test_unescape_shell_artifacts_preserves_other_backslashes() {
    assert_eq!(
        unescape_shell_artifacts("path\\to\\file and \\!"),
        "path\\to\\file and !"
    );
}

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
fn test_kanban_ci_status() {
    assert_eq!(kanban_ci_status(&[]), "unknown");
    assert_eq!(
        kanban_ci_status(&[serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})]),
        "passed"
    );
    assert_eq!(
        kanban_ci_status(&[serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})]),
        "failed"
    );
    assert_eq!(
        kanban_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
        "running"
    );
}

#[test]
fn test_hash_insight_deterministic() {
    let hash1 = hash_insight("Test insight content");
    let hash2 = hash_insight("Test insight content");
    assert_eq!(hash1, hash2);
}

#[test]
fn test_hash_insight_different_content() {
    let hash1 = hash_insight("Insight one");
    let hash2 = hash_insight("Insight two");
    assert_ne!(hash1, hash2);
}

#[test]
fn test_filter_coworkers_by_provider() {
    let coworkers = vec![
        crate::coworker::Coworker {
            slot_id: "1".to_string(),
            name: "lexington".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp/lexington".to_string(),
            started_at: chrono::Utc::now(),
            current_task: Some("Build auth".to_string()),
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
        },
        crate::coworker::Coworker {
            slot_id: "2".to_string(),
            name: "park".to_string(),
            status: crate::coworker::CoworkerStatus::Running,
            working_dir: "/tmp/park".to_string(),
            started_at: chrono::Utc::now(),
            current_task: Some("Review PR".to_string()),
            session_id: None,
            model: "gpt-5-codex".to_string(),
            provider: crate::auth::AuthProvider::Codex,
        },
    ];

    let claude = filter_coworkers_by_provider(&coworkers, crate::auth::AuthProvider::Claude);
    let codex = filter_coworkers_by_provider(&coworkers, crate::auth::AuthProvider::Codex);

    assert_eq!(claude.len(), 1);
    assert_eq!(claude[0].name, "lexington");
    assert_eq!(codex.len(), 1);
    assert_eq!(codex[0].name, "park");
}

#[test]
fn test_parse_provider_param_defaults_to_claude() {
    let provider = parse_provider_param(None).expect("should parse default provider");
    assert_eq!(provider, crate::auth::AuthProvider::Claude);
}

#[test]
fn test_parse_provider_param_parses_codex() {
    let params = serde_json::json!({ "provider": "codex" });
    let provider = parse_provider_param(Some(&params)).expect("should parse codex");
    assert_eq!(provider, crate::auth::AuthProvider::Codex);
}

#[test]
fn test_parse_provider_param_rejects_unknown_provider() {
    let params = serde_json::json!({ "provider": "unknown" });
    let err = parse_provider_param(Some(&params)).expect_err("provider should be rejected");
    assert!(err.contains("Unsupported provider"));
}

#[test]
fn test_build_coworker_relaunch_config_preserves_name_and_model() {
    let coworker = crate::coworker::Coworker {
        slot_id: "1".to_string(),
        name: "madison".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/madison".to_string(),
        started_at: chrono::Utc::now(),
        current_task: Some("Fix tests".to_string()),
        session_id: None,
        model: "opus".to_string(),
        provider: crate::auth::AuthProvider::Claude,
    };

    let config = build_coworker_relaunch_config(&coworker, "midtown");
    assert_eq!(config.name, "madison");
    assert_eq!(config.model, "opus");
    assert_eq!(config.session_mode, crate::launch::SessionMode::Resume);
}

#[test]
fn test_lead_relaunch_status_strings() {
    assert_eq!(LeadRelaunchStatus::Relaunched.as_str(), "relaunched");
    assert_eq!(LeadRelaunchStatus::Failed.as_str(), "failed");
    assert_eq!(LeadRelaunchStatus::Unchanged.as_str(), "unchanged");
    assert_eq!(LeadRelaunchStatus::Unchanged.summary(), "lead unchanged");
    assert!(!LeadRelaunchStatus::Unchanged.attempted());
    assert!(LeadRelaunchStatus::Relaunched.relaunched());
}

#[test]
fn test_hash_insight_normalizes_whitespace() {
    let hash1 = hash_insight("This is an insight");
    let hash2 = hash_insight("  This  is   an   insight  ");
    let hash3 = hash_insight("This\n  is\nan\ninsight");
    let hash4 = hash_insight("THIS IS AN INSIGHT");

    assert_eq!(hash1, hash2, "extra whitespace should be normalized");
    assert_eq!(hash1, hash3, "newlines should be normalized");
    assert_eq!(hash1, hash4, "case should be normalized");
}

#[test]
fn test_extract_review_note_pr_standard_format() {
    let msg = "@lead [Review Note] PR #708: The new is_ui_chrome() pattern for ctrl+ key hints is heuristic. Please determine if this warrants a follow-up task.";
    assert_eq!(extract_review_note_pr(msg), Some(708));
}

#[test]
fn test_extract_review_note_pr_no_match() {
    assert_eq!(extract_review_note_pr("@lead some regular message"), None);
    assert_eq!(extract_review_note_pr("fixed PR #42"), None);
    assert_eq!(extract_review_note_pr("[Review Note] no PR ref"), None);
}

#[test]
fn test_extract_review_note_pr_various_numbers() {
    assert_eq!(
        extract_review_note_pr("@lead [Review Note] PR #1: minor issue"),
        Some(1)
    );
    assert_eq!(
        extract_review_note_pr("@lead [Review Note] PR #9999: edge case"),
        Some(9999)
    );
}

// ---- Session attach target parsing tests ----

#[test]
fn test_parse_attach_target_name() {
    assert_eq!(
        parse_attach_target("name:park").unwrap(),
        AttachTarget::Name("park".to_string())
    );
    // Names are lowercased
    assert_eq!(
        parse_attach_target("name:Park").unwrap(),
        AttachTarget::Name("park".to_string())
    );
}

#[test]
fn test_parse_attach_target_name_empty() {
    assert!(parse_attach_target("name:").is_err());
}

#[test]
fn test_parse_attach_target_task() {
    assert_eq!(
        parse_attach_target("task:42").unwrap(),
        AttachTarget::Task(42)
    );
}

#[test]
fn test_parse_attach_target_task_invalid() {
    assert!(parse_attach_target("task:abc").is_err());
    assert!(parse_attach_target("task:-1").is_err());
}

#[test]
fn test_parse_attach_target_pr() {
    assert_eq!(
        parse_attach_target("pr:123").unwrap(),
        AttachTarget::Pr(123)
    );
}

#[test]
fn test_parse_attach_target_pr_invalid() {
    assert!(parse_attach_target("pr:abc").is_err());
}

#[test]
fn test_parse_attach_target_invalid_format() {
    assert!(parse_attach_target("invalid").is_err());
    assert!(parse_attach_target("unknown:value").is_err());
    assert!(parse_attach_target("").is_err());
}

// ---- RPC idempotency cache tests ----

/// Verify that the cache lookup logic correctly skips expired entries.
///
/// The cache in `handle_request` checks `now.duration_since(timestamp) < 60s`.
/// An entry older than 60 seconds should be treated as a cache miss, allowing
/// the request to re-execute (important for retries after transient failures).
#[test]
fn test_rpc_cache_ttl_expiration() {
    use crate::rpc::{RequestId, Response};

    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
    let request_id = RequestId::String("test-ttl-123".to_string());
    let cached_response = Response::success(request_id.clone(), serde_json::json!({"task_id": 42}));

    // Insert entry with a timestamp 61 seconds in the past
    let old_timestamp = Instant::now() - Duration::from_secs(61);
    cache.insert(request_id.clone(), (cached_response, old_timestamp));

    // Simulate the cache lookup from handle_request (lines 104-116)
    let now = Instant::now();
    let cache_hit = cache
        .get(&request_id)
        .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    assert!(
        cache_hit.is_none(),
        "Entry older than 60 seconds should be a cache miss"
    );
}

/// Verify that cache entries within TTL are returned as hits.
#[test]
fn test_rpc_cache_within_ttl() {
    use crate::rpc::{RequestId, Response};

    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
    let request_id = RequestId::String("test-fresh-456".to_string());
    let cached_response = Response::success(request_id.clone(), serde_json::json!({"task_id": 99}));

    // Insert entry with current timestamp (within TTL)
    cache.insert(request_id.clone(), (cached_response, Instant::now()));

    let now = Instant::now();
    let cache_hit = cache
        .get(&request_id)
        .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    assert!(cache_hit.is_some(), "Recent entry should be a cache hit");
}

/// Verify that cleanup_rpc_response_cache retains fresh entries and
/// removes expired ones — preventing unbounded memory growth.
#[test]
fn test_rpc_cache_cleanup_removes_expired_entries() {
    use crate::rpc::{RequestId, Response};

    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();

    // Add 100 expired entries
    let old_timestamp = Instant::now() - Duration::from_secs(120);
    for i in 0..100 {
        let id = RequestId::String(format!("expired-{}", i));
        let resp = Response::success(id.clone(), serde_json::json!({"i": i}));
        cache.insert(id, (resp, old_timestamp));
    }

    // Add 3 fresh entries
    let fresh_timestamp = Instant::now();
    for i in 0..3 {
        let id = RequestId::String(format!("fresh-{}", i));
        let resp = Response::success(id.clone(), serde_json::json!({"i": i}));
        cache.insert(id, (resp, fresh_timestamp));
    }

    assert_eq!(cache.len(), 103);

    // Simulate the cleanup logic from DaemonState::cleanup_rpc_response_cache
    let now = Instant::now();
    cache.retain(|_, (_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    assert_eq!(
        cache.len(),
        3,
        "Cleanup should remove all 100 expired entries, keeping 3 fresh ones"
    );

    // Verify only fresh entries remain
    for i in 0..3 {
        let id = RequestId::String(format!("fresh-{}", i));
        assert!(
            cache.contains_key(&id),
            "Fresh entry {} should be retained",
            i
        );
    }
}

/// Verify that only successful responses are cached (error responses are excluded).
///
/// This is important because caching errors would prevent retry-on-failure:
/// if a request fails due to a transient issue, retrying with the same request ID
/// should re-attempt the operation, not return the cached error.
#[test]
fn test_rpc_cache_only_caches_success_responses() {
    use crate::rpc::{RequestId, Response, RpcError};

    let success = Response::success(
        RequestId::String("s1".to_string()),
        serde_json::json!({"ok": true}),
    );
    let error = Response::error(
        RequestId::String("e1".to_string()),
        RpcError::invalid_params(),
    );

    // Reproduce the cache-insertion guard from handle_request (line 547)
    assert!(!success.is_error(), "Success response should not be error");
    assert!(error.is_error(), "Error response should be error");

    // Simulate: only cache non-error responses
    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();
    let responses = vec![success, error];

    for resp in &responses {
        // This mirrors the guard: `if !response.is_error()`
        if !resp.is_error() {
            cache.insert(
                RequestId::String("test".to_string()),
                (resp.clone(), Instant::now()),
            );
        }
    }

    assert_eq!(cache.len(), 1, "Only success response should be cached");
}

/// Verify that sequential numeric request IDs (as generated by the CLI)
/// would collide in the cache when coming from separate processes.
///
/// This is the regression test for the bug where `midtown task create`
/// called twice in quick succession returned the first task's response
/// both times, because both CLI processes sent `id: 1`.
#[test]
fn test_rpc_cache_numeric_id_collision() {
    use crate::rpc::{RequestId, Response};

    let mut cache: HashMap<RequestId, (Response, Instant)> = HashMap::new();

    // First CLI invocation sends id: 1, creates task !100
    let id_from_process_a = RequestId::Number(1);
    let response_a = Response::success(
        id_from_process_a.clone(),
        serde_json::json!({"task_id": 100}),
    );
    cache.insert(id_from_process_a.clone(), (response_a, Instant::now()));

    // Second CLI invocation also sends id: 1 (different process, counter restarted)
    let id_from_process_b = RequestId::Number(1);

    // This demonstrates the bug: same numeric ID = cache hit, wrong response
    let now = Instant::now();
    let cache_hit = cache
        .get(&id_from_process_b)
        .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    // With numeric IDs, this DOES hit — which is the bug.
    // The fix is to use unique string IDs (pid-counter) so this can't happen.
    assert!(
        cache_hit.is_some(),
        "Numeric ID collision: same id=1 from different processes hits cache (this is the bug)"
    );

    // After fix: string IDs with PID prefix won't collide
    let id_with_pid_a = RequestId::String("12345-1".to_string());
    let id_with_pid_b = RequestId::String("12346-1".to_string()); // different PID

    let response_a2 = Response::success(id_with_pid_a.clone(), serde_json::json!({"task_id": 100}));
    cache.insert(id_with_pid_a, (response_a2, Instant::now()));

    let cache_hit = cache
        .get(&id_with_pid_b)
        .filter(|(_, timestamp)| now.duration_since(*timestamp).as_secs() < 60);

    assert!(
        cache_hit.is_none(),
        "PID-prefixed string IDs from different processes should NOT collide"
    );
}

#[test]
fn test_apply_task_channel_mapping_sets_channel() {
    let mut map = HashMap::new();
    let changed = apply_task_channel_mapping(&mut map, "42", Some("auth"), false);
    assert!(changed);
    assert_eq!(map.get("42"), Some(&"auth".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_overwrites_existing() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "old-channel".to_string());
    let changed = apply_task_channel_mapping(&mut map, "42", Some("new-channel"), false);
    assert!(changed);
    assert_eq!(map.get("42"), Some(&"new-channel".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_ignores_none() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "auth".to_string());
    let changed = apply_task_channel_mapping(&mut map, "42", None, false);
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"auth".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_ignores_empty_without_clear() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "auth".to_string());
    // On create (allow_clear=false), empty string is ignored
    let changed = apply_task_channel_mapping(&mut map, "42", Some(""), false);
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"auth".to_string()));
}

#[test]
fn test_apply_task_channel_mapping_clears_with_empty_on_update() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "auth".to_string());
    // On update (allow_clear=true), empty string clears the mapping
    let changed = apply_task_channel_mapping(&mut map, "42", Some(""), true);
    assert!(changed);
    assert!(!map.contains_key("42"));
}

#[test]
fn test_apply_task_channel_mapping_clear_nonexistent_is_noop() {
    let mut map = HashMap::new();
    // Clearing a mapping that doesn't exist returns false (no state modification)
    let changed = apply_task_channel_mapping(&mut map, "99", Some(""), true);
    assert!(!changed);
    assert!(map.is_empty());
}

#[test]
fn test_apply_task_channel_mapping_none_on_empty_map() {
    let mut map: HashMap<String, String> = HashMap::new();
    let changed = apply_task_channel_mapping(&mut map, "42", None, true);
    assert!(!changed);
    assert!(map.is_empty());
}

#[test]
fn test_validate_model_format_valid() {
    assert!(validate_model_format("claude/opus").is_ok());
    assert!(validate_model_format("claude/sonnet").is_ok());
    assert!(validate_model_format("claude/haiku").is_ok());
    assert!(validate_model_format("codex/o3").is_ok());
    assert!(validate_model_format("codex/o4-mini").is_ok());
}

#[test]
fn test_validate_model_format_invalid() {
    // Missing slash
    assert!(validate_model_format("claude-opus").is_err());
    // Multiple slashes
    assert!(validate_model_format("claude/opus/extra").is_err());
    // Empty string
    assert!(validate_model_format("").is_err());
    // Only slash
    assert!(validate_model_format("/").is_err());
    // Empty provider
    assert!(validate_model_format("/opus").is_err());
    // Empty model
    assert!(validate_model_format("claude/").is_err());
    // Unsupported provider
    assert!(validate_model_format("unknown/opus").is_err());
    assert!(validate_model_format("openai/gpt4").is_err());
    // Whitespace in model or provider
    assert!(validate_model_format("claude/ opus").is_err());
    assert!(validate_model_format("claude /opus").is_err());
    assert!(validate_model_format(" claude/opus").is_err());
    assert!(validate_model_format("claude/opus ").is_err());
}

#[test]
fn test_apply_task_model_mapping_sets_model() {
    let mut map = HashMap::new();
    let changed = apply_task_model_mapping(&mut map, "42", Some("claude/opus"), false);
    assert!(changed.is_ok());
    assert!(changed.unwrap());
    assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
}

#[test]
fn test_apply_task_model_mapping_rejects_invalid_format() {
    let mut map = HashMap::new();
    let result = apply_task_model_mapping(&mut map, "42", Some("invalid-format"), false);
    assert!(result.is_err());
    assert!(map.is_empty());
}

#[test]
fn test_apply_task_model_mapping_overwrites_existing() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    let changed = apply_task_model_mapping(&mut map, "42", Some("claude/sonnet"), false).unwrap();
    assert!(changed);
    assert_eq!(map.get("42"), Some(&"claude/sonnet".to_string()));
}

#[test]
fn test_apply_task_model_mapping_ignores_none() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    let changed = apply_task_model_mapping(&mut map, "42", None, false).unwrap();
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
}

#[test]
fn test_apply_task_model_mapping_ignores_empty_without_clear() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    // On create (allow_clear=false), empty string is ignored
    let changed = apply_task_model_mapping(&mut map, "42", Some(""), false).unwrap();
    assert!(!changed);
    assert_eq!(map.get("42"), Some(&"claude/opus".to_string()));
}

#[test]
fn test_apply_task_model_mapping_clears_with_empty_on_update() {
    let mut map = HashMap::new();
    map.insert("42".to_string(), "claude/opus".to_string());
    // On update (allow_clear=true), empty string clears the mapping
    let changed = apply_task_model_mapping(&mut map, "42", Some(""), true).unwrap();
    assert!(changed);
    assert!(!map.contains_key("42"));
}

#[test]
fn test_apply_task_model_mapping_clear_nonexistent_is_noop() {
    let mut map = HashMap::new();
    // Clearing a mapping that doesn't exist returns false (no state modification)
    let changed = apply_task_model_mapping(&mut map, "99", Some(""), true).unwrap();
    assert!(!changed);
    assert!(map.is_empty());
}

#[test]
fn test_apply_task_model_mapping_none_on_empty_map() {
    let mut map: HashMap<String, String> = HashMap::new();
    let changed = apply_task_model_mapping(&mut map, "42", None, true).unwrap();
    assert!(!changed);
    assert!(map.is_empty());
}

/// Test that reviewers display source task IDs from PR titles instead of internal IDs.
///
/// When a coworker is reviewing a PR, the Board TUI should show the source task ID
/// (extracted from the PR title) rather than the reviewer's internal ephemeral task ID.
///
/// For example, if amsterdam is reviewing PR #968 (for task !1158), the display should
/// show !1158, not !62 (amsterdam's internal reviewer task ID).
#[test]
fn test_reviewer_displays_source_task_id() {
    use std::collections::HashMap;

    // Simulate the kanban_data logic for a reviewer scenario

    // Mock PR data: PR #968 is for task !1158 (in title)
    let pr_number: u64 = 968;
    let _pr_title = "Fix worktree sandbox issue [Midtown !1158]";
    let source_task_id: u32 = 1158;

    // Mock reviewer's internal task ID (ephemeral, not meaningful to user)
    let reviewer_internal_task_id: u32 = 62;

    // Build task_id_by_pr map (extracted from PR titles)
    // This is what handle_kanban_data does at line 2679
    let mut task_id_by_pr: HashMap<u64, u32> = HashMap::new();
    task_id_by_pr.insert(pr_number, source_task_id);

    // Build prs_by_task_id map (for authors to find their PRs)
    let mut prs_by_task_id: HashMap<u32, u64> = HashMap::new();
    prs_by_task_id.insert(source_task_id, pr_number);

    // Build reviewer_pr_map (reviewer name -> PR they're reviewing)
    // This comes from GitHub state
    let mut reviewer_pr_map: HashMap<String, u64> = HashMap::new();
    reviewer_pr_map.insert("amsterdam".to_string(), pr_number);

    // Simulate coworker data collection for a reviewer
    let coworker_name = "amsterdam";
    let task_id = Some(reviewer_internal_task_id); // Reviewer's internal task

    // This is the logic from handle_kanban_data lines 2744-2753
    // Find PR number for this coworker (either as reviewer or author)
    let pr_number_opt = reviewer_pr_map
        .get(coworker_name)
        .copied()
        .or_else(|| task_id.and_then(|tid| prs_by_task_id.get(&tid).copied()));

    // Prefer source task ID (from PR title) over internal task ID
    let display_task_id = pr_number_opt
        .and_then(|pr| task_id_by_pr.get(&pr).copied())
        .or(task_id);

    // Verify the display shows the source task ID, not the internal ID
    assert_eq!(
        display_task_id,
        Some(source_task_id),
        "Reviewer should display source task ID !{} from PR title, not internal task ID !{}",
        source_task_id,
        reviewer_internal_task_id
    );

    // Also verify we correctly found the PR for the reviewer
    assert_eq!(
        pr_number_opt,
        Some(pr_number),
        "Should find PR for reviewer"
    );

    // Verify the logic works correctly: the final display is the source task, not internal
    assert_ne!(
        display_task_id,
        Some(reviewer_internal_task_id),
        "Should NOT display reviewer's internal task ID"
    );
}

/// Verify handle_task_metadata uses async .lock().await (not blocking_lock()).
///
/// Before the fix, handle_task_metadata used blocking_lock() on a tokio::Mutex
/// inside an async context, causing "Cannot block the current thread" panics.
/// This test confirms the function works correctly in a tokio runtime.
#[tokio::test]
async fn test_handle_task_metadata_uses_async_lock() {
    use crate::daemon::DaemonState;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let base_dir = temp_dir.path().to_path_buf();
    let wm = crate::worktree::WorktreeManager::new(base_dir.clone()).expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new("test-session", wm);
    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    std::mem::forget(temp_dir);

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test-metadata.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![base_dir],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");

    // This would panic with "Cannot block the current thread from within
    // a runtime" if handle_task_metadata still used blocking_lock().
    let response = handle_task_metadata(RequestId::Number(1), "nonexistent-task", &state).await;

    // Should return success with null channel/model for nonexistent task
    assert!(response.error.is_none(), "Expected success response");
    let result = response.result.expect("Expected result in response");
    assert!(result["channel"].is_null());
    assert!(result["model"].is_null());
}

/// Verify handle_task_done uses async .lock().await (not blocking_lock()).
///
/// Before the fix, handle_task_done used blocking_lock() on a tokio::Mutex
/// inside an async context, causing deadlocks when persistent_state was held
/// by collect_world_snapshot() on the same tokio runtime.
#[tokio::test]
async fn test_handle_task_done_uses_async_lock() {
    use crate::daemon::DaemonState;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let base_dir = temp_dir.path().to_path_buf();
    let wm = crate::worktree::WorktreeManager::new(base_dir.clone()).expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new("test-session", wm);
    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    std::mem::forget(temp_dir);

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test-task-done.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![base_dir],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");

    // This would deadlock if handle_task_done still used blocking_lock()
    // on the tokio::Mutex, since the single-threaded tokio test runtime
    // can't make progress when a thread is blocked.
    let response = handle_task_done(RequestId::Number(1), "nonexistent-task", &state).await;

    // Should return error for nonexistent task (task file doesn't exist)
    assert!(
        response.error.is_some(),
        "Expected error for nonexistent task"
    );
}

/// Reproduce the bug: `blocking_lock()` on a `tokio::Mutex` inside an async
/// context panics with "Cannot block the current thread from within a runtime."
///
/// In production, this manifested as daemon crashes when `collect_world_snapshot`
/// and RPC handlers (handle_task_done, pr_action_to_effects, etc.) contended
/// for `persistent_state`. The fix replaces all `blocking_lock()` calls with
/// `.lock().await`.
///
/// Part 1 proves `blocking_lock()` panics in async context (the bug).
/// Part 2 proves `.lock().await` works under the same contention (the fix).
#[tokio::test(flavor = "current_thread")]
async fn test_blocking_lock_deadlock_reproduction() {
    use std::sync::Arc;

    let mutex = Arc::new(tokio::sync::Mutex::new(42u32));

    // === Part 1: blocking_lock() panics in async context ===
    // On a current_thread runtime, tokio detects the illegal blocking call
    // and panics. On a multi_thread runtime with contention, it deadlocks
    // instead. Either way, blocking_lock() is wrong in async code.

    let m1 = Arc::clone(&mutex);
    let blocker = tokio::spawn(async move {
        let _guard = m1.blocking_lock();
    });

    let blocker_result = blocker.await;
    assert!(
        blocker_result.is_err(),
        "Expected blocking_lock() to panic inside tokio runtime, but it succeeded"
    );
    let panic_msg = format!("{:?}", blocker_result.unwrap_err());
    assert!(
        panic_msg.contains("block the current thread"),
        "Expected 'Cannot block the current thread' panic, got: {}",
        panic_msg
    );

    // === Part 2: .lock().await works under contention (the fix) ===

    let m2 = Arc::clone(&mutex);
    let holder = tokio::spawn(async move {
        let _guard = m2.lock().await;
        // Simulate collect_world_snapshot doing work while holding the lock.
        tokio::task::yield_now().await;
    });

    let m3 = Arc::clone(&mutex);
    let awaiter = tokio::spawn(async move {
        // .lock().await yields cooperatively — holder completes and releases
        // the lock, then awaiter acquires it. No deadlock.
        let _guard = m3.lock().await;
    });

    let ok_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        holder.await.expect("holder panicked");
        awaiter.await.expect("awaiter panicked");
    })
    .await;

    assert!(
        ok_result.is_ok(),
        "Expected .lock().await to resolve without deadlock"
    );
}

/// Ensure no daemon code uses blocking_lock() on tokio::Mutex.
///
/// blocking_lock() in async context causes deadlocks when the tokio runtime
/// can't schedule the lock holder. This has caused daemon crashes multiple times
/// (PR #1045 fixed handle_task_metadata, this PR fixed handle_task_done + pr.rs).
/// This test prevents regression by scanning the daemon source files.
#[test]
fn no_blocking_lock_in_daemon_code() {
    let daemon_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon");
    let mut violations = Vec::new();

    for entry in std::fs::read_dir(&daemon_dir).expect("read daemon dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "rs") {
            // Skip separate test files (they contain test code)
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("_tests.rs"))
            {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("read file");
            let mut in_test_module = false;
            for (line_num, line) in content.lines().enumerate() {
                // Skip comments
                if line.trim_start().starts_with("//") || line.trim_start().starts_with("///") {
                    continue;
                }
                // Track #[cfg(test)] module boundaries
                if line.contains("#[cfg(test)]") {
                    in_test_module = true;
                    continue;
                }
                if in_test_module {
                    continue;
                }
                let needle = format!(".{}()", "blocking_lock");
                if line.contains(&needle) {
                    violations.push(format!(
                        "{}:{}: {}",
                        path.file_name().unwrap().to_string_lossy(),
                        line_num + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found blocking_lock() calls in daemon code (use .lock().await instead):\n{}",
        violations.join("\n")
    );
}
