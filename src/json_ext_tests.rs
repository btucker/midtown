use super::*;
use serde_json::json;

#[test]
fn str_field_returns_string() {
    let v = json!({"name": "alice"});
    assert_eq!(v.str_field("name"), Some("alice"));
    assert_eq!(v.str_field("missing"), None);
}

#[test]
fn str_or_returns_default() {
    let v = json!({"name": "alice"});
    assert_eq!(v.str_or("name", "default"), "alice");
    assert_eq!(v.str_or("missing", "default"), "default");
}

#[test]
fn u64_field_returns_number() {
    let v = json!({"count": 42});
    assert_eq!(v.u64_field("count"), Some(42));
    assert_eq!(v.u64_field("missing"), None);
}

#[test]
fn bool_field_and_bool_or() {
    let v = json!({"active": true});
    assert_eq!(v.bool_field("active"), Some(true));
    assert!(v.bool_or("active", false));
    assert!(!v.bool_or("missing", false));
}

#[test]
fn array_and_object_fields() {
    let v = json!({"items": [1, 2], "meta": {"k": "v"}});
    assert!(v.array_field("items").is_some());
    assert!(v.object_field("meta").is_some());
    assert!(v.array_field("missing").is_none());
}

#[test]
fn type_mismatch_returns_none() {
    let v = json!({"name": "alice"});
    assert_eq!(v.u64_field("name"), None);
    assert_eq!(v.bool_field("name"), None);
}
