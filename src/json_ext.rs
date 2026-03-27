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

#[path = "json_ext_tests.rs"]
#[cfg(test)]
mod tests;
