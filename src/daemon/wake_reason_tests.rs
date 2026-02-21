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
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("lexington"));
    assert!(msg.contains("Auth module needs refactoring"));
}

#[test]
fn nudge_passthrough() {
    let reason = WakeReason::Nudge {
        message: "Check PR #99".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("Check PR #99"));
}
