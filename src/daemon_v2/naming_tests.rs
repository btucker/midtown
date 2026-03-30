use super::*;
use std::collections::HashSet;

// ── Section 13: Naming ──────────────────────────────────────────────────────

/// Spec 13: WHEN generating an agent name AND none is provided THEN a random
/// adjective-noun combination SHALL be used
#[test]
fn generates_adjective_noun_combination() {
    let existing = HashSet::new();
    let name = generate_name(&existing);
    let parts: Vec<&str> = name.split('-').collect();
    assert_eq!(
        parts.len(),
        2,
        "name should be adjective-noun with one hyphen: {name}"
    );
    assert!(
        ADJECTIVES.contains(&parts[0]),
        "first word should be an adjective: {}",
        parts[0]
    );
    assert!(
        NOUNS.contains(&parts[1]),
        "second word should be a noun: {}",
        parts[1]
    );
}

/// Spec 13: WHEN the generated name already exists THEN generation SHALL retry
#[test]
fn retries_when_name_exists() {
    let mut existing = HashSet::new();
    // Generate several names and ensure uniqueness
    for _ in 0..20 {
        let name = generate_name(&existing);
        assert!(
            !existing.contains(&name),
            "generated name {name} was already in use"
        );
        existing.insert(name);
    }
}

/// Spec 13: WHEN all retries are exhausted THEN fallback name agent-{random 4-digit}
/// SHALL be used
#[test]
fn fallback_name_when_all_combinations_taken() {
    // Fill all possible adjective-noun combinations
    let mut existing = HashSet::new();
    for adj in ADJECTIVES {
        for noun in NOUNS {
            existing.insert(format!("{adj}-{noun}"));
        }
    }

    let name = generate_name(&existing);
    assert!(
        name.starts_with("agent-"),
        "fallback should start with 'agent-': {name}"
    );
    let suffix = &name["agent-".len()..];
    let num: u32 = suffix
        .parse()
        .unwrap_or_else(|_| panic!("fallback suffix should be numeric: {suffix}"));
    assert!(
        (1000..10000).contains(&num),
        "fallback number should be 4 digits: {num}"
    );
}

#[test]
fn random_icon_is_valid() {
    let icon = random_icon();
    assert!(!icon.is_empty());
}

#[test]
fn random_color_is_valid() {
    let color = random_color();
    assert!(color.starts_with('#'), "color should start with #: {color}");
}
