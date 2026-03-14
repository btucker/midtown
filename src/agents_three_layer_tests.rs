//! Tests for the three-layer prompt architecture.
//!
//! Layer 1: Agent definition (role identity from agent definition files)
//! Layer 2: Shared prompt (common.md, lead-common.md)
//! Layer 3: Runtime context (name, project, channel, template var replacement)

use super::*;

// ── Layer 1: load_agent_definition_for_role ──────────────────────

#[test]
fn test_layer1_coworker_returns_code_author_content() {
    let content = load_agent_definition_for_role(AgentRole::Coworker);
    assert!(
        content.contains("Code Author"),
        "Coworker Layer 1 should load midtown-code-author content"
    );
    assert!(
        content.contains("code author"),
        "Coworker Layer 1 should contain code author identity"
    );
}

#[test]
fn test_layer1_reviewer_returns_code_reviewer_content() {
    let content = load_agent_definition_for_role(AgentRole::Reviewer);
    assert!(
        content.contains("Code Reviewer"),
        "Reviewer Layer 1 should load midtown-code-reviewer content"
    );
    assert!(
        content.contains("THRESHOLD OVERRIDE"),
        "Reviewer Layer 1 should contain review-specific instructions"
    );
}

#[test]
fn test_layer1_project_lead_returns_project_lead_content() {
    let content = load_agent_definition_for_role(AgentRole::ProjectLead);
    assert!(
        content.contains("Project Lead"),
        "ProjectLead Layer 1 should load midtown-project-lead content"
    );
    assert!(
        content.contains("human-facing"),
        "ProjectLead Layer 1 should mention being human-facing"
    );
}

#[test]
fn test_layer1_channel_lead_returns_channel_lead_content() {
    let content = load_agent_definition_for_role(AgentRole::ChannelLead);
    assert!(
        content.contains("Channel Lead"),
        "ChannelLead Layer 1 should load midtown-channel-lead content"
    );
    assert!(
        content.contains("domain expert"),
        "ChannelLead Layer 1 should mention being a domain expert"
    );
}

#[test]
fn test_layer1_agent_definitions_have_no_template_vars() {
    for role in [
        AgentRole::Coworker,
        AgentRole::Reviewer,
        AgentRole::ProjectLead,
        AgentRole::ChannelLead,
    ] {
        let content = load_agent_definition_for_role(role);
        assert!(
            !content.contains("{name}"),
            "{role:?} agent definition should not contain {{name}} template var"
        );
        assert!(
            !content.contains("{project_name}"),
            "{role:?} agent definition should not contain {{project_name}} template var"
        );
        assert!(
            !content.contains("{channel_lead}"),
            "{role:?} agent definition should not contain {{channel_lead}} template var"
        );
    }
}

// ── Layer 2: shared_prompt_for_role ──────────────────────────────

#[test]
fn test_layer2_coworker_returns_common_only() {
    let shared = shared_prompt_for_role(AgentRole::Coworker);
    assert!(
        shared.contains("GitHub Etiquette"),
        "Coworker shared prompt should include common.md content"
    );
    assert!(
        shared.contains("Team Roles"),
        "Coworker shared prompt should include Team Roles from common.md"
    );
    // Should NOT include lead-specific content
    assert!(
        !shared.contains("Lead Coordination"),
        "Coworker shared prompt should NOT include lead-common.md content"
    );
}

#[test]
fn test_layer2_reviewer_returns_common_only() {
    let shared = shared_prompt_for_role(AgentRole::Reviewer);
    assert!(
        shared.contains("GitHub Etiquette"),
        "Reviewer shared prompt should include common.md content"
    );
    assert!(
        !shared.contains("Lead Coordination"),
        "Reviewer shared prompt should NOT include lead-common.md content"
    );
}

#[test]
fn test_layer2_project_lead_returns_lead_common_and_common() {
    let shared = shared_prompt_for_role(AgentRole::ProjectLead);
    assert!(
        shared.contains("Lead Coordination"),
        "ProjectLead shared prompt should include lead-common.md content"
    );
    assert!(
        shared.contains("GitHub Etiquette"),
        "ProjectLead shared prompt should include common.md content"
    );
}

#[test]
fn test_layer2_channel_lead_returns_lead_common_and_common() {
    let shared = shared_prompt_for_role(AgentRole::ChannelLead);
    assert!(
        shared.contains("Lead Coordination"),
        "ChannelLead shared prompt should include lead-common.md content"
    );
    assert!(
        shared.contains("GitHub Etiquette"),
        "ChannelLead shared prompt should include common.md content"
    );
}

// ── Layer 3: build_runtime_context ───────────────────────────────

#[test]
fn test_layer3_replaces_name() {
    let input = "Hello {name}, welcome to {project_name}";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "broadway",
            project_name: "midtown",
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("Hello broadway"));
    assert!(result.contains("welcome to midtown"));
    assert!(!result.contains("{name}"));
    assert!(!result.contains("{project_name}"));
}

#[test]
fn test_layer3_replaces_channel_lead_with_provided_value() {
    let input = "Ask @{channel_lead} for help";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "park",
            project_name: "midtown",
            channel_lead: Some("web"),
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("Ask @web for help"));
}

#[test]
fn test_layer3_channel_lead_defaults_to_project_name() {
    let input = "Ask @{channel_lead} for help";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "park",
            project_name: "midtown",
            channel_lead: None,
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("Ask @midtown for help"));
}

#[test]
fn test_layer3_replaces_channel_name() {
    let input = "Channel: #{channel_name}";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "web",
            project_name: "midtown",
            channel_name: Some("web-interface"),
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("Channel: #web-interface"));
}

#[test]
fn test_layer3_replaces_domain_context() {
    let input = "Context: {domain_context}";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "web",
            project_name: "midtown",
            domain_context: Some("Active tasks: !42"),
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("Context: Active tasks: !42"));
}

#[test]
fn test_layer3_replaces_escalation_target() {
    let input = "@{escalation_target} [Review Note]";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "york",
            project_name: "midtown",
            escalation_target: Some("daemon-core"),
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("@daemon-core [Review Note]"));
}

#[test]
fn test_layer3_appends_agents_md() {
    let input = "Base prompt";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "web",
            project_name: "midtown",
            agents_md: Some("Use `/study` to begin research."),
            ..RuntimeContext::default()
        },
    );
    assert!(result.contains("## Workflow Facilitation"));
    assert!(result.contains("Use `/study` to begin research."));
}

#[test]
fn test_layer3_skips_empty_agents_md() {
    let input = "Base prompt";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "web",
            project_name: "midtown",
            agents_md: Some("  \n  "),
            ..RuntimeContext::default()
        },
    );
    assert!(!result.contains("## Workflow Facilitation"));
}

#[test]
fn test_layer3_preserves_literal_placeholders_in_agents_md() {
    let input = "Base prompt";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "web",
            project_name: "midtown",
            agents_md: Some("Use {name} as the template variable"),
            ..RuntimeContext::default()
        },
    );
    assert!(
        result.contains("Use {name} as the template variable"),
        "Literal {{name}} in AGENTS.md should be preserved"
    );
}

#[test]
fn test_layer3_appends_ops_extra_for_ops_channel() {
    let input = "Base prompt";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "ops",
            project_name: "midtown",
            channel_name: Some("ops"),
            ..RuntimeContext::default()
        },
    );
    // ops-channel-lead.md contains specific operational instructions
    assert!(
        result.contains("PR lifecycle"),
        "Ops channel should get ops-channel-lead.md content with 'PR lifecycle' section"
    );
}

#[test]
fn test_layer3_ops_extras_have_template_vars_replaced() {
    let input = "Base prompt with {name}";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "ops",
            project_name: "midtown",
            channel_name: Some("ops"),
            ..RuntimeContext::default()
        },
    );
    // Template vars in ops extras should be replaced (unlike AGENTS.md)
    assert!(
        !result.contains("{name}"),
        "Ops extras should have {{name}} replaced, but AGENTS.md should not"
    );
}

#[test]
fn test_layer3_no_ops_extra_for_non_ops_channel() {
    let input = "Base prompt";
    let result = build_runtime_context(
        input,
        &RuntimeContext {
            name: "web",
            project_name: "midtown",
            channel_name: Some("web"),
            ..RuntimeContext::default()
        },
    );
    assert!(
        !result.contains("PR lifecycle"),
        "Non-ops channel should NOT get ops-channel-lead.md content"
    );
}

// ── Section ordering tests ───────────────────────────────────────

#[test]
fn test_reviewer_prompt_common_md_before_reviewer_instructions() {
    let prompt = reviewer_system_prompt(
        "york",
        "midtown",
        "midtown",
        crate::auth::AuthProvider::Claude,
        Some(42),
    );
    let common_pos = prompt
        .find("GitHub Etiquette")
        .expect("common.md content should be present");
    let reviewer_pos = prompt
        .find("## Reviewer Instructions")
        .expect("Reviewer Instructions section should be present");
    assert!(
        common_pos < reviewer_pos,
        "common.md content must appear BEFORE '## Reviewer Instructions' \
         (common at {common_pos}, reviewer at {reviewer_pos})"
    );
}

// ── Full assembly regression tests ───────────────────────────────

#[test]
fn test_assembled_coworker_prompt_has_all_layers() {
    let prompt = coworker_system_prompt("lexington", "midtown", None);
    // Layer 1: Agent definition content
    assert!(
        prompt.contains("Code Author") || prompt.contains("coworker"),
        "Assembled coworker prompt should contain Layer 1 content"
    );
    // Layer 2: Common content
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Assembled coworker prompt should contain Layer 2 common content"
    );
    // Layer 3: Runtime substitutions applied
    assert!(
        !prompt.contains("{name}"),
        "Assembled coworker prompt should have no unreplaced template vars"
    );
}

#[test]
fn test_assembled_reviewer_prompt_has_all_layers() {
    let prompt = reviewer_system_prompt(
        "york",
        "midtown",
        "midtown",
        crate::auth::AuthProvider::Claude,
        Some(42),
    );
    // Layer 1: Reviewer definition content
    assert!(
        prompt.contains("Code Reviewer") || prompt.contains("THRESHOLD OVERRIDE"),
        "Assembled reviewer prompt should contain Layer 1 reviewer content"
    );
    // Layer 2: Common content (reviewers also need operational rules)
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Assembled reviewer prompt should contain Layer 2 common content"
    );
    // Layer 3: Runtime substitutions
    assert!(
        !prompt.contains("{name}"),
        "Assembled reviewer prompt should have no unreplaced template vars"
    );
}

#[test]
fn test_assembled_lead_prompt_has_all_layers() {
    let prompt = main_lead_system_prompt("midtown");
    // Layer 1: Project lead definition
    assert!(
        prompt.contains("Project Lead"),
        "Assembled lead prompt should contain Layer 1 content"
    );
    // Layer 2: Lead-common + common
    assert!(
        prompt.contains("Lead Coordination"),
        "Assembled lead prompt should contain Layer 2 lead-common content"
    );
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Assembled lead prompt should contain Layer 2 common content"
    );
    // Layer 3: Runtime substitutions
    assert!(
        !prompt.contains("{name}"),
        "Assembled lead prompt should have no unreplaced template vars"
    );
    assert!(
        !prompt.contains("{project_name}"),
        "Assembled lead prompt should have no unreplaced {{project_name}}"
    );
}

#[test]
fn test_assembled_channel_lead_prompt_has_all_layers() {
    let prompt = channel_lead_system_prompt("web-interface", "Active tasks.", "midtown", None);
    // Layer 1: Channel lead definition
    assert!(
        prompt.contains("Channel Lead") || prompt.contains("channel lead"),
        "Assembled channel lead prompt should contain Layer 1 content"
    );
    // Layer 2: Lead-common + common
    assert!(
        prompt.contains("Lead Coordination"),
        "Assembled channel lead prompt should contain Layer 2 lead-common content"
    );
    assert!(
        prompt.contains("GitHub Etiquette"),
        "Assembled channel lead prompt should contain Layer 2 common content"
    );
    // Layer 3: Runtime substitutions
    assert!(
        !prompt.contains("{channel_name}"),
        "Assembled channel lead prompt should have no unreplaced {{channel_name}}"
    );
}
