use super::*;
use crate::auth::AuthProvider;

#[test]
fn test_main_lead_system_prompt_loads() {
    let prompt = main_lead_system_prompt("midtown");
    assert!(prompt.contains("Lead Coordination"));
    assert!(prompt.contains("midtown"));
}

#[test]
fn test_coworker_system_prompt_substitutes_name() {
    let prompt = coworker_system_prompt("lexington", "midtown");
    assert!(prompt.contains("**lexington**"));
    assert!(prompt.contains("git checkout -b lexington/"));
    assert!(!prompt.contains("{name}"));
}

#[test]
fn test_coworker_system_prompt_contains_required_sections() {
    let prompt = coworker_system_prompt("park", "midtown");
    assert!(prompt.contains("Channel Usage"));
    assert!(prompt.contains("Your Task"));
    assert!(prompt.contains("Git Workflow"));
    assert!(prompt.contains("Coordination"));
    assert!(prompt.contains("Read the Channel"));
    assert!(prompt.contains("midtown channel read"));
    assert!(prompt.contains("[Midtown !"));
}

#[test]
fn test_lead_prompt_contains_commands() {
    let prompt = main_lead_system_prompt("midtown");
    assert!(prompt.contains("midtown status"));
    assert!(prompt.contains("midtown coworker call-in"));
    assert!(prompt.contains("midtown channel"));
}

#[test]
fn test_lead_prompt_contains_delegation_section() {
    let prompt = main_lead_system_prompt("midtown");
    assert!(prompt.contains("Delegation Mindset"));
    assert!(prompt.contains("coordinator"));
}

#[test]
fn test_common_prompt_included_in_lead() {
    let prompt = main_lead_system_prompt("midtown");
    assert!(
        prompt.contains("CRITICAL: NEVER use @mentions in GitHub"),
        "Lead prompt should include GitHub @mentions rule from common.md"
    );
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Lead prompt should include GitHub Etiquette section from common.md"
    );
    assert!(
        prompt.contains("Insights"),
        "Lead prompt should include Insights section from common.md"
    );
}

#[test]
fn test_common_prompt_included_in_coworker() {
    let prompt = coworker_system_prompt("park", "midtown");
    assert!(
        prompt.contains("CRITICAL: NEVER use @mentions in GitHub"),
        "Coworker prompt should include GitHub @mentions rule from common.md"
    );
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Coworker prompt should include GitHub Etiquette section from common.md"
    );
    assert!(
        prompt.contains("Insights"),
        "Coworker prompt should include Insights section from common.md"
    );
}

#[test]
fn test_common_prompt_name_substitution_in_lead() {
    let prompt = main_lead_system_prompt("midtown");
    assert!(
        prompt.contains("<!-- midtown: midtown -->"),
        "Lead prompt should have {{name}} replaced with project_name in common content"
    );
    assert!(
        !prompt.contains("{name}"),
        "Lead prompt should not contain unreplaced {{name}} placeholders"
    );
}

#[test]
fn test_common_prompt_name_substitution_in_coworker() {
    let prompt = coworker_system_prompt("broadway", "midtown");
    assert!(
        prompt.contains("<!-- midtown: broadway -->"),
        "Coworker prompt should have {{name}} replaced in common content"
    );
}

#[test]
fn test_reviewer_prompt_substitutes_pr_number() {
    let prompt = reviewer_prompt(42, AuthProvider::Claude);
    assert!(prompt.contains("reviewing PR #42"));
    assert!(prompt.contains("/code-review:code-review 42"));
    assert!(prompt.contains("gh pr comment 42 --body"));
    assert!(prompt.contains("PR #42 repeats"));
    assert!(
        !prompt.contains("{pr_number}"),
        "Reviewer prompt should not contain unreplaced {{pr_number}} placeholders"
    );
}

#[test]
fn test_reviewer_resume_prompt_substitutes_pr_number() {
    let prompt = reviewer_resume_prompt(99, AuthProvider::Claude);
    assert!(prompt.contains("Resume reviewing PR #99"));
    assert!(prompt.contains("gh pr comment 99 --body"));
    assert!(prompt.contains("PR #99 repeats"));
    assert!(
        !prompt.contains("{pr_number}"),
        "Reviewer resume prompt should not contain unreplaced {{pr_number}} placeholders"
    );
}

#[test]
fn test_reviewer_prompt_contains_required_sections() {
    let prompt = reviewer_prompt(1, AuthProvider::Claude);
    assert!(
        prompt.contains("IMPORTANT"),
        "Reviewer prompt should contain IMPORTANT section"
    );
    assert!(
        prompt.contains("REFACTOR DETECTION"),
        "Reviewer prompt should contain REFACTOR DETECTION section"
    );
    assert!(
        prompt.contains("TASK DESCRIPTION VERIFICATION"),
        "Reviewer prompt should contain TASK DESCRIPTION VERIFICATION section"
    );
}

#[test]
fn test_reviewer_prompt_codex_invocation() {
    let prompt = reviewer_prompt(42, AuthProvider::Codex);
    assert!(
        prompt.contains("use the code-review skill to review PR #42"),
        "Codex reviewer prompt should use the skill instruction"
    );
    assert!(
        !prompt.contains("/code-review:code-review 42"),
        "Codex reviewer prompt should not use slash command"
    );
}

#[test]
fn test_reviewer_resume_prompt_contains_task_verification() {
    let prompt = reviewer_resume_prompt(1, AuthProvider::Claude);
    assert!(
        prompt.contains("TASK DESCRIPTION VERIFICATION"),
        "Reviewer resume prompt should contain TASK DESCRIPTION VERIFICATION section"
    );
    assert!(
        prompt.contains("/code-review:code-review"),
        "Reviewer resume prompt should contain explicit code-review skill invocation"
    );
}

#[test]
fn test_reviewer_system_prompt_merges_all_sources() {
    let prompt = reviewer_system_prompt("lexington", "midtown", AuthProvider::Claude, Some(42));

    // Should contain content from common.md
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Reviewer system prompt should include common.md content"
    );

    // Should contain content from coworker.md
    assert!(
        prompt.contains("Channel Usage"),
        "Reviewer system prompt should include coworker.md content"
    );

    // Should contain reviewer-specific instructions from reviewer.md
    assert!(
        prompt.contains("THRESHOLD OVERRIDE"),
        "Reviewer system prompt should include THRESHOLD OVERRIDE from reviewer.md"
    );
    assert!(
        prompt.contains("CHANNEL MESSAGE DISCIPLINE"),
        "Reviewer system prompt should include CHANNEL MESSAGE DISCIPLINE from reviewer.md"
    );
    assert!(
        prompt.contains("TEST SUGGESTIONS"),
        "Reviewer system prompt should include TEST SUGGESTIONS from reviewer.md"
    );

    // Should have name substituted
    assert!(
        prompt.contains("**lexington**"),
        "Reviewer system prompt should substitute {{name}} with actual name"
    );
    assert!(
        !prompt.contains("{name}"),
        "Reviewer system prompt should not contain unreplaced {{name}}"
    );
}

#[test]
fn test_reviewer_prompts_include_frontmatter_requirement() {
    let system_prompt = reviewer_system_prompt("park", "midtown", AuthProvider::Claude, Some(42));
    let resume_prompt = reviewer_resume_prompt(42, AuthProvider::Claude);

    assert!(
        system_prompt.contains("MIDTOWN FRONTMATTER REQUIREMENT"),
        "Reviewer system prompt should contain MIDTOWN FRONTMATTER REQUIREMENT section"
    );
    assert!(
        system_prompt.contains("<!-- midtown: park -->"),
        "Reviewer system prompt should show the frontmatter format with substituted name"
    );
    assert!(
        system_prompt.contains("prepend the frontmatter"),
        "Reviewer system prompt should instruct to prepend frontmatter"
    );

    assert!(
        resume_prompt.contains("MIDTOWN FRONTMATTER REQUIREMENT"),
        "Reviewer resume prompt should contain MIDTOWN FRONTMATTER REQUIREMENT section"
    );
}

#[test]
fn test_channel_lead_system_prompt_substitutes_channel_name() {
    let prompt = channel_lead_system_prompt("web-interface", "No context yet.", "midtown");
    assert!(
        prompt.contains("#web-interface"),
        "Channel lead prompt should contain the channel name with # prefix"
    );
    assert!(
        prompt.contains("--channel web-interface"),
        "Channel lead prompt should show correct channel flag"
    );
    assert!(
        !prompt.contains("{channel_name}"),
        "Channel lead prompt should not contain unreplaced {{channel_name}} placeholders"
    );
}

#[test]
fn test_channel_lead_system_prompt_substitutes_domain_context() {
    let context = "Active tasks: !42 Add WebSocket reconnect. Recent PRs: #99 merged.";
    let prompt = channel_lead_system_prompt("daemon-core", context, "midtown");
    assert!(
        prompt.contains(context),
        "Channel lead prompt should inject domain context"
    );
    assert!(
        !prompt.contains("{domain_context}"),
        "Channel lead prompt should not contain unreplaced {{domain_context}} placeholder"
    );
}

#[test]
fn test_channel_lead_system_prompt_contains_required_sections() {
    let prompt = channel_lead_system_prompt("tui", "No context.", "midtown");
    assert!(
        prompt.contains("Identity"),
        "Channel lead prompt should have Identity section"
    );
    assert!(
        prompt.contains("Escalation"),
        "Channel lead prompt should have Escalation section"
    );
    assert!(
        prompt.contains("Living Documents"),
        "Channel lead prompt should have Living Documents section"
    );
    assert!(
        prompt.contains("midtown channel post"),
        "Channel lead prompt should describe how to post"
    );
}

#[test]
fn test_channel_lead_system_prompt_contains_escalation_to_lead() {
    let prompt = channel_lead_system_prompt("github-integration", "No context.", "midtown");
    assert!(
        prompt.contains("@midtown"),
        "Channel lead prompt should mention @midtown for escalation"
    );
}

#[test]
fn test_coworker_prompt_prevents_orphaned_branches() {
    let prompt = coworker_system_prompt("park", "midtown");

    assert!(
        prompt.contains("Before pushing"),
        "Coworker prompt should contain 'Before pushing' section"
    );
    assert!(
        prompt.contains("gh pr list --search"),
        "Coworker prompt should instruct to check for existing PRs by task number"
    );
    assert!(
        prompt.contains("force-push to the existing PR branch"),
        "Coworker prompt should instruct to force-push to existing PR branch"
    );
    assert!(
        prompt.contains("Never create a new branch or new PR"),
        "Coworker prompt should warn against creating duplicate PRs"
    );

    assert!(
        prompt.contains("First, check if the PR is still open"),
        "Coworker prompt should instruct to check PR state before addressing feedback"
    );
    assert!(
        prompt.contains("gh pr view <number> --json state"),
        "Coworker prompt should show command to check PR state"
    );
    assert!(
        prompt.contains("Delete the orphaned remote branch"),
        "Coworker prompt should instruct to clean up orphaned branches"
    );
    assert!(
        prompt.contains("git push origin --delete"),
        "Coworker prompt should show command to delete remote branches"
    );
}

#[test]
fn test_coworker_prompt_requires_issue_comment_reviews() {
    let prompt = coworker_system_prompt("park", "midtown");

    assert!(
        prompt.contains("**Before merging**, complete ALL of these checks"),
        "Coworker prompt should call out the explicit pre-merge checklist"
    );
    assert!(
        prompt.contains(r#"gh api "repos/$repo/issues/<PR_NUMBER>/comments""#),
        "Coworker prompt should show the issue-comment gh api command using the repo shorthand"
    );
    assert!(
        prompt.contains("<!-- midtown:"),
        "Coworker prompt should mention the frontmatter signature so reviewers are detected"
    );
    assert!(
        prompt.contains(r#"midtown channel read | grep -i "don't merge\|do not merge\|hold\|stop.*merge\|<PR_NUMBER>""#),
        "Coworker prompt should instruct checking the channel for 'do not merge' directives"
    );
    assert!(
        prompt.contains(r#"gh pr view <number> --comments --json comments"#),
        "Coworker prompt should instruct checking recent PR comments for late requests"
    );
    assert!(
        prompt.contains("never merge while anything remains unresolved"),
        "Coworker prompt should emphasize no merging with unresolved review feedback"
    );
    assert!(
        prompt.contains("stop immediately"),
        "Coworker prompt should instruct stopping immediately when lead/user says not to merge"
    );
    assert!(
        prompt.contains("Only after all three checks are clean"),
        "Coworker prompt should gate auto-merge on all three pre-merge checks passing"
    );
}

#[test]
fn test_main_lead_initial_prompt_structure() {
    let prompt = main_lead_initial_prompt("midtown", "main");
    assert!(prompt.contains("## Role"));
    assert!(prompt.contains("Project Lead for midtown"));
    assert!(prompt.contains("## Channel"));
    assert!(prompt.contains("#main"));
    assert!(prompt.contains("## Mission"));
    assert!(prompt.contains("## First Actions"));
}

#[test]
fn test_channel_lead_initial_prompt_structure() {
    let prompt = channel_lead_initial_prompt("web-interface");
    assert!(prompt.contains("## Role"));
    assert!(prompt.contains("Channel lead for #web-interface"));
    assert!(prompt.contains("## Channel"));
    assert!(prompt.contains("#web-interface"));
    assert!(prompt.contains("## Mission"));
    assert!(prompt.contains("## First Actions"));
}

#[test]
fn test_coworker_task_prompt_contains_id_and_subject() {
    let prompt = coworker_task_prompt("42", "Fix login bug", "");
    assert!(prompt.contains("task !42"));
    assert!(prompt.contains("Fix login bug"));
    assert!(prompt.contains("Get started!"));
    assert!(prompt.contains("midtown task view 42"));
}

#[test]
fn test_coworker_task_prompt_includes_plan_section() {
    let plan = "\n\n## Plan Context\nSome plan details here.";
    let prompt = coworker_task_prompt("42", "Fix login bug", plan);
    assert!(prompt.contains("## Plan Context"));
    assert!(prompt.contains("Some plan details here."));
}

#[test]
fn test_coworker_claim_prompt_includes_claim_command() {
    let prompt = coworker_claim_prompt("42", "Fix login bug", "");
    assert!(prompt.contains("midtown task claim 42"));
    assert!(prompt.contains("task !42"));
}

#[test]
fn test_coworker_recovery_prompt_mentions_intact_worktree() {
    let prompt = coworker_recovery_prompt("42", "Fix login bug", "");
    assert!(prompt.contains("worktree and branch are still intact"));
    assert!(prompt.contains("git status"));
    assert!(prompt.contains("task !42"));
}

#[test]
fn test_coworker_nudge_prompt_is_brief() {
    let prompt = coworker_nudge_prompt("42", "Fix login bug");
    assert!(prompt.contains("pending task !42"));
    assert!(prompt.contains("Fix login bug"));
    assert!(prompt.contains("Get started!"));
}

#[test]
fn test_reviewer_launch_prompt_first_attempt_is_simple() {
    // restart_count=0: first attempt — simple "Review PR #N" command, no context
    let prompt = reviewer_launch_prompt(99, 0, AuthProvider::Claude);
    assert!(
        prompt.contains("/code-review:code-review 99"),
        "First attempt should include the code-review slash command"
    );
    assert!(
        !prompt.contains("NOTE (Restart"),
        "First attempt should NOT include restart context"
    );
    assert!(
        !prompt.contains("placeholder"),
        "First attempt should NOT mention placeholder"
    );
}

#[test]
fn test_reviewer_launch_prompt_restart_includes_context() {
    // restart_count>0: respawn — should include context about previous attempt
    let prompt = reviewer_launch_prompt(42, 1, AuthProvider::Claude);
    assert!(
        prompt.contains("/code-review:code-review 42"),
        "Respawn should still include the code-review slash command"
    );
    assert!(
        prompt.contains("NOTE (Restart #1)"),
        "Respawn should note the restart number"
    );
    assert!(
        prompt.contains("Review in progress"),
        "Respawn should mention the placeholder comment"
    );
    assert!(
        prompt.contains("review-pr-42"),
        "Respawn should mention the review worktree"
    );
}

#[test]
fn test_reviewer_launch_prompt_codex_invocation() {
    let prompt = reviewer_launch_prompt(42, 0, AuthProvider::Codex);
    assert!(
        prompt.contains("use the code-review skill to review PR #42"),
        "Codex reviewer launch prompt should mention code-review skill command"
    );
    assert!(
        !prompt.contains("/code-review"),
        "Codex launch prompt should avoid slash command"
    );
}
