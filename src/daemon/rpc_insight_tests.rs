//! Tests for insight RPC handlers.

use super::hash_insight;

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
fn test_hash_insight_normalizes_whitespace() {
    let hash1 = hash_insight("This is an insight");
    let hash2 = hash_insight("  This  is   an   insight  ");
    let hash3 = hash_insight("This\n  is\nan\ninsight");
    let hash4 = hash_insight("THIS IS AN INSIGHT");

    assert_eq!(hash1, hash2, "extra whitespace should be normalized");
    assert_eq!(hash1, hash3, "newlines should be normalized");
    assert_eq!(hash1, hash4, "case should be normalized");
}
