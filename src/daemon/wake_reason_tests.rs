use super::WakeReason;

#[test]
fn task_created_nudge_message() {
    let reason = WakeReason::TaskCreated {
        task_id: "42".to_string(),
        subject: "Fix auth bug".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("!42"), "should contain task ID");
    assert!(msg.contains("Fix auth bug"), "should contain subject");
}

#[test]
fn task_created_initial_prompt() {
    let reason = WakeReason::TaskCreated {
        task_id: "42".to_string(),
        subject: "Fix auth bug".to_string(),
    };
    let prompt = reason.to_initial_prompt("web");
    assert!(prompt.contains("Channel lead for #web"));
    assert!(prompt.contains("!42"));
    assert!(prompt.contains("Fix auth bug"));
    assert!(prompt.contains("midtown task view 42"));
}

#[test]
fn user_message_nudge_message() {
    let reason = WakeReason::UserMessage {
        content: "What's the status?".to_string(),
        msg_id: "msg-abc".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("msg-abc"), "should contain msg_id");
    assert!(msg.contains("What's the status?"), "should contain content");
}

#[test]
fn user_message_initial_prompt() {
    let reason = WakeReason::UserMessage {
        content: "What's the status?".to_string(),
        msg_id: "msg-abc".to_string(),
    };
    let prompt = reason.to_initial_prompt("ops");
    assert!(prompt.contains("Channel lead for #ops"));
    assert!(prompt.contains("What's the status?"));
}

#[test]
fn insight_posted_nudge_message() {
    let reason = WakeReason::InsightPosted {
        insight: "Auth module needs refactoring".to_string(),
        agent: "lexington".to_string(),
        msg_id: "msg-xyz".to_string(),
        task_id: Some("42".to_string()),
        channel_name: "daemon-core".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("lexington"), "should contain agent name");
    assert!(msg.contains("!42"), "should contain task reference");
    assert!(msg.contains("#daemon-core"), "should contain channel name");
    assert!(
        msg.contains("Auth module needs refactoring"),
        "should contain insight content"
    );
    assert!(
        msg.contains("ONLY reply"),
        "should include explicit 'ONLY reply' instruction"
    );
    assert!(
        msg.contains("--thread msg-xyz"),
        "should include thread reply command with msg_id"
    );
    assert!(
        msg.contains("--channel daemon-core"),
        "should include --channel flag in reply command"
    );
}

#[test]
fn insight_posted_nudge_message_without_task_id() {
    let reason = WakeReason::InsightPosted {
        insight: "Interesting pattern".to_string(),
        agent: "broadway".to_string(),
        msg_id: "msg-abc".to_string(),
        task_id: None,
        channel_name: "web".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("broadway"));
    assert!(
        !msg.contains("!"),
        "no task ID should appear without task_id"
    );
    assert!(msg.contains("#web"), "should contain channel name");
    assert!(
        msg.contains("ONLY reply"),
        "should include ONLY reply instruction"
    );
}

#[test]
fn insight_posted_initial_prompt() {
    let reason = WakeReason::InsightPosted {
        insight: "Cache invalidation is complex here".to_string(),
        agent: "lexington".to_string(),
        msg_id: "msg-xyz".to_string(),
        task_id: Some("99".to_string()),
        channel_name: "backend".to_string(),
    };
    let prompt = reason.to_initial_prompt("backend");
    assert!(prompt.contains("Channel lead for #backend"));
    assert!(prompt.contains("lexington"));
    assert!(prompt.contains("!99"));
    assert!(prompt.contains("ONLY reply"));
    assert!(prompt.contains("--thread msg-xyz"));
}

#[test]
fn nudge_passthrough() {
    let reason = WakeReason::Nudge {
        message: "Check PR #99".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("Check PR #99"));
}
