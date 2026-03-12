use super::{ThreadContext, WakeReason};

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
    assert!(
        prompt.contains("--task 42"),
        "TaskCreated initial prompt should include --task reply instruction"
    );
}

#[test]
fn user_message_nudge_message() {
    let reason = WakeReason::UserMessage {
        content: "What's the status?".to_string(),
        msg_id: "msg-abc".to_string(),
        thread_ctx: None,
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
        thread_ctx: None,
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
    assert!(
        msg.contains("save it to your notes"),
        "should include save-to-notes reminder for domain knowledge"
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
    assert!(
        msg.contains("save it to your notes"),
        "should include save-to-notes reminder"
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
    assert!(
        prompt.contains("save it to your notes"),
        "initial prompt should include save-to-notes reminder for domain knowledge"
    );
}

#[test]
fn task_nudges_include_reply_instruction() {
    // All task-related wake reasons should include --task reply instruction
    let assigned = WakeReason::TaskAssigned {
        task_id: "7".to_string(),
        subject: "Build widget".to_string(),
    };
    assert!(
        assigned.to_nudge_message().contains("--task 7"),
        "TaskAssigned nudge should include --task reply instruction"
    );

    let claimed = WakeReason::TaskClaimed {
        task_id: "7".to_string(),
        subject: "Build widget".to_string(),
        plan_section: String::new(),
    };
    assert!(
        claimed.to_nudge_message().contains("--task 7"),
        "TaskClaimed nudge should include --task reply instruction"
    );

    let recovery = WakeReason::SessionRecovery {
        task_id: "7".to_string(),
        subject: "Build widget".to_string(),
    };
    assert!(
        recovery.to_nudge_message().contains("--task 7"),
        "SessionRecovery nudge should include --task reply instruction"
    );

    let created = WakeReason::TaskCreated {
        task_id: "7".to_string(),
        subject: "Build widget".to_string(),
    };
    assert!(
        created.to_nudge_message().contains("--task 7"),
        "TaskCreated nudge should include --task reply instruction"
    );
}

#[test]
fn review_assigned_nudge_uses_rich_template() {
    let reason = WakeReason::ReviewAssigned { pr_number: 42 };
    let msg = reason.to_nudge_message();
    assert!(
        msg.contains("Resume reviewing PR #42"),
        "ReviewAssigned should use the full reviewer-resume template, not a one-liner"
    );
    // Posting instructions are in the system prompt, not the resume nudge
    assert!(
        msg.contains("system prompt"),
        "ReviewAssigned should reference the system prompt for behavioral instructions"
    );
    // Resume should carry actionable content: the code review skill invocation
    assert!(
        msg.contains("code-review"),
        "ReviewAssigned should include the code-review skill invocation"
    );
}

#[test]
fn task_claimed_nudge_includes_plan_section() {
    let reason = WakeReason::TaskClaimed {
        task_id: "7".to_string(),
        subject: "Build widget".to_string(),
        plan_section: "\n\n## Execution Skill\n\n**Use the `superpowers:deploy` skill.**"
            .to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(
        msg.contains("Execution Skill"),
        "TaskClaimed nudge should include plan section when provided"
    );
    assert!(
        msg.contains("superpowers:deploy"),
        "TaskClaimed nudge should include execution skill from plan section"
    );
}

#[test]
fn nudge_passthrough() {
    let reason = WakeReason::Nudge {
        message: "Check PR #99".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("Check PR #99"));
}

#[test]
fn dm_from_user_nudge_message_contains_content_and_reply_instruction() {
    let reason = WakeReason::DmFromUser {
        content: "Hey, can you check the auth module?".to_string(),
        msg_id: "msg-dm-001".to_string(),
        coworker_name: "madison".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(msg.contains("msg-dm-001"), "should contain msg_id");
    assert!(
        msg.contains("Hey, can you check the auth module?"),
        "should contain message content"
    );
    assert!(
        msg.contains("--channel dm-madison"),
        "should include reply channel instruction"
    );
}

#[test]
fn dm_from_user_nudge_message_reply_instruction_uses_coworker_name() {
    let reason = WakeReason::DmFromUser {
        content: "quick question".to_string(),
        msg_id: "msg-dm-002".to_string(),
        coworker_name: "amsterdam".to_string(),
    };
    let msg = reason.to_nudge_message();
    assert!(
        msg.contains("dm-amsterdam"),
        "reply instruction should reference the coworker's DM channel"
    );
}

#[test]
fn sender_returns_midtown_for_system_nudges() {
    assert_eq!(
        WakeReason::TaskAssigned {
            task_id: "1".into(),
            subject: "s".into()
        }
        .sender(),
        "midtown"
    );
    assert_eq!(
        WakeReason::TaskClaimed {
            task_id: "1".into(),
            subject: "s".into(),
            plan_section: String::new(),
        }
        .sender(),
        "midtown"
    );
    assert_eq!(
        WakeReason::SessionRecovery {
            task_id: "1".into(),
            subject: "s".into()
        }
        .sender(),
        "midtown"
    );
    assert_eq!(
        WakeReason::ReviewAssigned { pr_number: 42 }.sender(),
        "midtown"
    );
    assert_eq!(
        WakeReason::Nudge {
            message: "hi".into()
        }
        .sender(),
        "midtown"
    );
    assert_eq!(
        WakeReason::TaskCreated {
            task_id: "1".into(),
            subject: "s".into()
        }
        .sender(),
        "midtown"
    );
}

#[test]
fn sender_returns_from_for_mention() {
    let reason = WakeReason::Mention {
        from: "lexington".to_string(),
        content: "check this".to_string(),
        msg_id: "msg-1".to_string(),
        thread_ctx: None,
    };
    assert_eq!(reason.sender(), "lexington");
}

#[test]
fn sender_returns_agent_for_insight() {
    let reason = WakeReason::InsightPosted {
        insight: "interesting".into(),
        agent: "broadway".into(),
        msg_id: "msg-1".into(),
        task_id: None,
        channel_name: "ops".into(),
    };
    assert_eq!(reason.sender(), "broadway");
}

#[test]
fn sender_returns_user_for_user_messages() {
    assert_eq!(
        WakeReason::UserMessage {
            content: "hi".into(),
            msg_id: "m".into(),
            thread_ctx: None,
        }
        .sender(),
        "user"
    );
    assert_eq!(
        WakeReason::DmFromUser {
            content: "hi".into(),
            msg_id: "m".into(),
            coworker_name: "park".into()
        }
        .sender(),
        "user"
    );
}

#[test]
fn already_in_dm_channel_only_for_dm_from_user() {
    assert!(
        WakeReason::DmFromUser {
            content: "hi".into(),
            msg_id: "m".into(),
            coworker_name: "park".into()
        }
        .already_in_dm_channel()
    );

    // All others should return false
    assert!(
        !WakeReason::TaskAssigned {
            task_id: "1".into(),
            subject: "s".into()
        }
        .already_in_dm_channel()
    );
    assert!(
        !WakeReason::Mention {
            from: "lex".into(),
            content: "c".into(),
            msg_id: "m".into(),
            thread_ctx: None,
        }
        .already_in_dm_channel()
    );
    assert!(
        !WakeReason::Nudge {
            message: "hi".into()
        }
        .already_in_dm_channel()
    );
}

#[test]
fn user_message_thread_reply_nudge_includes_instructions() {
    let reason = WakeReason::UserMessage {
        content: "Can you fix this?".to_string(),
        msg_id: "msg-reply-001".to_string(),
        thread_ctx: Some(ThreadContext {
            parent_id: "parent-msg-uuid".to_string(),
            channel_name: "auth-refactor".to_string(),
        }),
    };
    let msg = reason.to_nudge_message();
    assert!(
        msg.contains("--thread parent-msg-uuid"),
        "thread reply nudge should include --thread instruction"
    );
    assert!(
        msg.contains("--channel auth-refactor"),
        "thread reply nudge should include --channel instruction"
    );
    assert!(
        msg.contains("This is a thread reply"),
        "thread reply nudge should indicate it's a thread reply"
    );
    assert!(
        msg.contains("channel read --last 50"),
        "thread reply nudge should use --last 50 for channel read"
    );
    assert!(
        msg.contains("text output auto-posts"),
        "thread reply nudge should include output suppression reminder"
    );
}

#[test]
fn mention_without_thread_ctx_formats_simple_message() {
    let reason = WakeReason::Mention {
        from: "broadway".to_string(),
        content: "check the auth flow".to_string(),
        msg_id: "msg-mention-001".to_string(),
        thread_ctx: None,
    };
    let msg = reason.to_nudge_message();
    assert!(
        msg.contains(
            "broadway mentioned you (channel-msg-id: msg-mention-001): check the auth flow"
        ),
        "should format as simple mention"
    );
    assert!(
        !msg.contains("thread reply"),
        "non-thread mention should not include thread instructions"
    );
}

#[test]
fn mention_with_thread_ctx_includes_thread_instructions() {
    let reason = WakeReason::Mention {
        from: "broadway".to_string(),
        content: "check the auth flow".to_string(),
        msg_id: "msg-mention-002".to_string(),
        thread_ctx: Some(ThreadContext {
            parent_id: "parent-thread-uuid".to_string(),
            channel_name: "daemon-core".to_string(),
        }),
    };
    let msg = reason.to_nudge_message();
    assert!(
        msg.contains("broadway mentioned you"),
        "should contain mention attribution"
    );
    assert!(
        msg.contains("--thread parent-thread-uuid"),
        "thread reply mention should include --thread instruction"
    );
    assert!(
        msg.contains("--channel daemon-core"),
        "thread reply mention should include --channel instruction"
    );
    assert!(
        msg.contains("This is a thread reply"),
        "thread reply mention should indicate it's a thread reply"
    );
}

#[test]
fn nudge_type_returns_correct_variant_names() {
    assert_eq!(
        WakeReason::TaskCreated {
            task_id: "1".into(),
            subject: "s".into()
        }
        .nudge_type(),
        "task_created"
    );
    assert_eq!(
        WakeReason::TaskAssigned {
            task_id: "1".into(),
            subject: "s".into()
        }
        .nudge_type(),
        "task_assigned"
    );
    assert_eq!(
        WakeReason::TaskClaimed {
            task_id: "1".into(),
            subject: "s".into(),
            plan_section: String::new(),
        }
        .nudge_type(),
        "task_claimed"
    );
    assert_eq!(
        WakeReason::SessionRecovery {
            task_id: "1".into(),
            subject: "s".into()
        }
        .nudge_type(),
        "session_recovery"
    );
    assert_eq!(
        WakeReason::ReviewAssigned { pr_number: 42 }.nudge_type(),
        "review_assigned"
    );
    assert_eq!(
        WakeReason::Mention {
            from: "lex".into(),
            content: "c".into(),
            msg_id: "m".into(),
            thread_ctx: None,
        }
        .nudge_type(),
        "mention"
    );
    assert_eq!(
        WakeReason::Nudge {
            message: "hi".into()
        }
        .nudge_type(),
        "nudge"
    );
    assert_eq!(
        WakeReason::UserMessage {
            content: "hi".into(),
            msg_id: "m".into(),
            thread_ctx: None,
        }
        .nudge_type(),
        "user_message"
    );
    assert_eq!(
        WakeReason::InsightPosted {
            insight: "i".into(),
            agent: "a".into(),
            msg_id: "m".into(),
            task_id: None,
            channel_name: "c".into(),
        }
        .nudge_type(),
        "insight_posted"
    );
    assert_eq!(
        WakeReason::DmFromUser {
            content: "hi".into(),
            msg_id: "m".into(),
            coworker_name: "park".into()
        }
        .nudge_type(),
        "dm_from_user"
    );
}

#[test]
fn user_message_thread_reply_initial_prompt_includes_instructions() {
    let reason = WakeReason::UserMessage {
        content: "Can you fix this?".to_string(),
        msg_id: "msg-reply-002".to_string(),
        thread_ctx: Some(ThreadContext {
            parent_id: "parent-msg-uuid".to_string(),
            channel_name: "auth-refactor".to_string(),
        }),
    };
    let prompt = reason.to_initial_prompt("auth-refactor");
    assert!(
        prompt.contains("--thread parent-msg-uuid"),
        "thread reply initial prompt should include --thread instruction"
    );
    assert!(
        prompt.contains("--channel auth-refactor"),
        "thread reply initial prompt should include --channel instruction"
    );
}

#[test]
fn reply_instructions_channel_read_includes_thread_flag() {
    // The `channel read` command in reply_instructions must include --thread
    // so the lead/fork reads the THREAD context, not the main channel.
    let ctx = ThreadContext {
        parent_id: "parent-123".to_string(),
        channel_name: "ops".to_string(),
    };
    let instructions = ctx.reply_instructions();

    // Extract the `channel read` command from the instructions
    let read_cmd = instructions
        .lines()
        .find(|l| l.contains("channel read"))
        .expect("reply_instructions should contain a channel read command");

    // Must contain --thread <parent_id> to read thread context, not main channel
    assert!(
        read_cmd.contains("--thread parent-123"),
        "channel read command must include --thread <parent_id>: {read_cmd}"
    );
    assert!(
        read_cmd.contains("--channel ops"),
        "channel read command should include --channel: {read_cmd}"
    );
}
