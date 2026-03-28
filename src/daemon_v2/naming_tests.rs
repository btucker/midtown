use super::*;
use std::collections::HashSet;

#[test]
fn generates_two_word_name() {
    let existing = HashSet::new();
    let name = generate_name(&existing);
    assert!(name.contains('-'), "name should be hyphenated: {name}");
    assert!(!name.is_empty());
}

#[test]
fn avoids_existing_names() {
    let mut existing = HashSet::new();
    let first = generate_name(&existing);
    existing.insert(first.clone());
    let second = generate_name(&existing);
    assert_ne!(first, second);
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
