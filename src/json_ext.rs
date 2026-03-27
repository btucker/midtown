//! Extension trait for ergonomic JSON field access.
//!
//! Reduces the common `.get("key").and_then(|v| v.as_str())` chain
//! to `.str_field("key")`.

use serde_json::Value;

/// Extension methods on [`serde_json::Value`] for concise field access.
pub trait ValueExt {
    fn str_field(&self, key: &str) -> Option<&str>;
    fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str;
    fn u64_field(&self, key: &str) -> Option<u64>;
    fn f64_field(&self, key: &str) -> Option<f64>;
    fn bool_field(&self, key: &str) -> Option<bool>;
    fn bool_or(&self, key: &str, default: bool) -> bool;
    fn array_field(&self, key: &str) -> Option<&Vec<Value>>;
    fn object_field(&self, key: &str) -> Option<&serde_json::Map<String, Value>>;
}

impl ValueExt for Value {
    fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }

    fn str_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.str_field(key).unwrap_or(default)
    }

    fn u64_field(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.as_u64())
    }

    fn f64_field(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(|v| v.as_f64())
    }

    fn bool_field(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(|v| v.as_bool())
    }

    fn bool_or(&self, key: &str, default: bool) -> bool {
        self.bool_field(key).unwrap_or(default)
    }

    fn array_field(&self, key: &str) -> Option<&Vec<Value>> {
        self.get(key).and_then(|v| v.as_array())
    }

    fn object_field(&self, key: &str) -> Option<&serde_json::Map<String, Value>> {
        self.get(key).and_then(|v| v.as_object())
    }
}

#[cfg(test)]
mod tests {
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
}
