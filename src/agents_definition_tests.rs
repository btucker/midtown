use std::path::Path;

use crate::agent_definition::parse_agent_content;

/// All agent definition files that must be valid.
const DEFINITIONS: &[(&str, &str)] = &[
    (
        "midtown-code-author",
        include_str!("../agents/definitions/midtown-code-author.md"),
    ),
    (
        "midtown-code-reviewer",
        include_str!("../agents/definitions/midtown-code-reviewer.md"),
    ),
    (
        "midtown-project-lead",
        include_str!("../agents/definitions/midtown-project-lead.md"),
    ),
    (
        "midtown-channel-lead",
        include_str!("../agents/definitions/midtown-channel-lead.md"),
    ),
];

/// Template variables that must NOT appear in definition files.
const FORBIDDEN_VARS: &[&str] = &[
    "{name}",
    "{project_name}",
    "{channel_name}",
    "{channel_lead}",
    "{escalation_target}",
    "{pr_number}",
    "{code_review_invocation}",
    "{domain_context}",
];

#[test]
fn all_definitions_parse_successfully() {
    for (filename, content) in DEFINITIONS {
        let path = Path::new(filename).with_extension("md");
        let result = parse_agent_content(content, &path);
        assert!(
            result.is_ok(),
            "Failed to parse {}: {}",
            filename,
            result.unwrap_err()
        );
    }
}

#[test]
fn all_definitions_have_valid_frontmatter() {
    for (filename, content) in DEFINITIONS {
        let path = Path::new(filename).with_extension("md");
        let def = parse_agent_content(content, &path).unwrap();

        assert_eq!(
            def.name, *filename,
            "{}: name should match filename",
            filename
        );

        assert!(
            def.description.is_some(),
            "{}: must have a description",
            filename
        );

        let desc = def.description.as_ref().unwrap();
        assert!(
            !desc.is_empty(),
            "{}: description must not be empty",
            filename
        );
    }
}

#[test]
fn no_template_variables_in_definitions() {
    for (filename, content) in DEFINITIONS {
        for var in FORBIDDEN_VARS {
            assert!(
                !content.contains(var),
                "{} contains forbidden template variable: {}",
                filename,
                var
            );
        }
    }
}

#[test]
fn all_definitions_have_substantive_content() {
    for (filename, content) in DEFINITIONS {
        let path = Path::new(filename).with_extension("md");
        let def = parse_agent_content(content, &path).unwrap();

        assert!(
            def.system_prompt.len() > 100,
            "{}: system prompt is too short ({} chars) — expected substantive role-specific content",
            filename,
            def.system_prompt.len()
        );
    }
}

#[test]
fn code_author_contains_role_keywords() {
    let content = include_str!("../agents/definitions/midtown-code-author.md");
    let path = Path::new("midtown-code-author.md");
    let def = parse_agent_content(content, path).unwrap();

    let keywords = ["coworker", "worktree", "branch", "PR", "commit"];
    for keyword in keywords {
        assert!(
            def.system_prompt.contains(keyword),
            "code-author should contain '{}' in system prompt",
            keyword
        );
    }
}

#[test]
fn code_reviewer_contains_role_keywords() {
    let content = include_str!("../agents/definitions/midtown-code-reviewer.md");
    let path = Path::new("midtown-code-reviewer.md");
    let def = parse_agent_content(content, path).unwrap();

    let keywords = ["review", "PR", "threshold"];
    for keyword in keywords {
        assert!(
            def.system_prompt.contains(keyword),
            "code-reviewer should contain '{}' in system prompt",
            keyword
        );
    }
}

#[test]
fn project_lead_contains_role_keywords() {
    let content = include_str!("../agents/definitions/midtown-project-lead.md");
    let path = Path::new("midtown-project-lead.md");
    let def = parse_agent_content(content, path).unwrap();

    let keywords = ["Project Lead", "user", "delegate"];
    for keyword in keywords {
        assert!(
            def.system_prompt.contains(keyword),
            "project-lead should contain '{}' in system prompt",
            keyword
        );
    }
}

#[test]
fn channel_lead_contains_role_keywords() {
    let content = include_str!("../agents/definitions/midtown-channel-lead.md");
    let path = Path::new("midtown-channel-lead.md");
    let def = parse_agent_content(content, path).unwrap();

    let keywords = ["channel lead", "domain"];
    for keyword in keywords {
        assert!(
            def.system_prompt.contains(keyword),
            "channel-lead should contain '{}' in system prompt",
            keyword
        );
    }
}
