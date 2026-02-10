//! Session key type for identifying coworker sessions.
//!
//! `SessionKey` is the compound key that identifies a specific coworker session.
//! It pairs a display name (avenue name like "lexington") with a Claude Code
//! session UUID, enabling multiple concurrent sessions per coworker name.
//!
//! **Design principle**: Names are a UX layer; session IDs are the real identity.
//! Two sessions with the same name but different IDs are independent sessions.
//! The name is preserved for channel messages, web UI, and @mentions.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Compound key for coworker sessions.
///
/// Combines a human-readable display name (avenue name) with a unique session
/// identifier. This allows multiple concurrent sessions per coworker name while
/// maintaining human-readable channel output.
///
/// # Identity semantics
///
/// - **Hash and Eq use `session_id` only** — the session_id is the true identity
/// - **Display uses `name` only** — for channel messages and human-readable output
/// - **Serialization preserves both** — `"name:session_id"` string format
///
/// # Examples
///
/// ```
/// use midtown::SessionKey;
///
/// let key = SessionKey::new("lexington", "abc-123-def");
/// assert_eq!(key.name(), "lexington");
/// assert_eq!(key.session_id(), "abc-123-def");
/// assert_eq!(key.to_string(), "lexington");
/// assert_eq!(key.full_id(), "lexington:abc-123-def");
/// ```
#[derive(Debug, Clone)]
pub struct SessionKey {
    /// Coworker display name (avenue name like "lexington", "park", etc.)
    name: String,
    /// Claude Code session UUID — unique per session
    session_id: String,
}

impl SessionKey {
    /// Create a new session key.
    pub fn new(name: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            session_id: session_id.into(),
        }
    }

    /// The coworker display name (e.g., "lexington").
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The Claude Code session UUID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Full identifier for internal keying: `"lexington:abc-123-def"`.
    pub fn full_id(&self) -> String {
        format!("{}:{}", self.name, self.session_id)
    }

    /// Parse a full ID string back into a SessionKey.
    ///
    /// Expects the format `"name:session_id"`. Returns `None` if the string
    /// doesn't contain a colon separator, or if either part is empty.
    pub fn parse(s: &str) -> Option<Self> {
        let colon_pos = s.find(':')?;
        let name = &s[..colon_pos];
        let session_id = &s[colon_pos + 1..];

        if name.is_empty() || session_id.is_empty() {
            return None;
        }

        Some(Self {
            name: name.to_string(),
            session_id: session_id.to_string(),
        })
    }

    /// Create a SessionKey from just a name, generating a random session ID.
    ///
    /// Used when a session ID isn't available yet (e.g., during recovery or
    /// when the headless session hasn't reported its init event).
    pub fn with_generated_id(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            session_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

// ── Identity: session_id is the true key ──────────────────────────────

impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
    }
}

impl Eq for SessionKey {}

impl std::hash::Hash for SessionKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.session_id.hash(state);
    }
}

// ── Display: name is for humans ───────────────────────────────────────

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

// ── Serialization: "name:session_id" string format ────────────────────

impl Serialize for SessionKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.full_id())
    }
}

impl<'de> Deserialize<'de> for SessionKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        SessionKey::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid SessionKey format: expected 'name:session_id', got '{}'",
                s
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_new_and_accessors() {
        let key = SessionKey::new("lexington", "abc-123-def");
        assert_eq!(key.name(), "lexington");
        assert_eq!(key.session_id(), "abc-123-def");
    }

    #[test]
    fn test_full_id() {
        let key = SessionKey::new("park", "xyz-789");
        assert_eq!(key.full_id(), "park:xyz-789");
    }

    #[test]
    fn test_display_shows_name_only() {
        let key = SessionKey::new("broadway", "session-42");
        assert_eq!(format!("{}", key), "broadway");
        assert_eq!(key.to_string(), "broadway");
    }

    #[test]
    fn test_parse_valid() {
        let key = SessionKey::parse("lexington:abc-123-def").unwrap();
        assert_eq!(key.name(), "lexington");
        assert_eq!(key.session_id(), "abc-123-def");
    }

    #[test]
    fn test_parse_with_colons_in_session_id() {
        // Session IDs from Claude Code may contain hyphens but not colons.
        // However, if they did, the first colon is the separator.
        let key = SessionKey::parse("park:abc:def:ghi").unwrap();
        assert_eq!(key.name(), "park");
        assert_eq!(key.session_id(), "abc:def:ghi");
    }

    #[test]
    fn test_parse_invalid_no_colon() {
        assert!(SessionKey::parse("lexington").is_none());
    }

    #[test]
    fn test_parse_invalid_empty_name() {
        assert!(SessionKey::parse(":abc-123").is_none());
    }

    #[test]
    fn test_parse_invalid_empty_session_id() {
        assert!(SessionKey::parse("lexington:").is_none());
    }

    #[test]
    fn test_eq_uses_session_id_only() {
        let key1 = SessionKey::new("lexington", "session-1");
        let key2 = SessionKey::new("park", "session-1"); // different name, same session_id
        let key3 = SessionKey::new("lexington", "session-2"); // same name, different session_id

        assert_eq!(
            key1, key2,
            "Same session_id should be equal regardless of name"
        );
        assert_ne!(
            key1, key3,
            "Different session_id should not be equal even with same name"
        );
    }

    #[test]
    fn test_hash_uses_session_id_only() {
        use std::collections::HashSet;

        let key1 = SessionKey::new("lexington", "session-1");
        let key2 = SessionKey::new("park", "session-1"); // different name, same session_id

        let mut set = HashSet::new();
        set.insert(key1);
        assert!(
            set.contains(&key2),
            "Should find key by session_id regardless of name"
        );
    }

    #[test]
    fn test_hashmap_key() {
        let key1 = SessionKey::new("lexington", "session-1");
        let key2 = SessionKey::new("park", "session-2");

        let mut map = HashMap::new();
        map.insert(key1.clone(), "task-42");
        map.insert(key2.clone(), "task-43");

        assert_eq!(map.get(&key1), Some(&"task-42"));
        assert_eq!(map.get(&key2), Some(&"task-43"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn test_serde_roundtrip() {
        let key = SessionKey::new("broadway", "abc-def-123");
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(json, r#""broadway:abc-def-123""#);

        let parsed: SessionKey = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name(), "broadway");
        assert_eq!(parsed.session_id(), "abc-def-123");
    }

    #[test]
    fn test_serde_in_hashmap() {
        let mut map = HashMap::new();
        map.insert(SessionKey::new("park", "s1"), "task-1".to_string());
        map.insert(SessionKey::new("york", "s2"), "task-2".to_string());

        let json = serde_json::to_string(&map).unwrap();
        let parsed: HashMap<SessionKey, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_serde_deserialize_error() {
        let result: Result<SessionKey, _> = serde_json::from_str(r#""no-colon-here""#);
        assert!(result.is_err());
    }

    #[test]
    fn test_with_generated_id() {
        let key = SessionKey::with_generated_id("madison");
        assert_eq!(key.name(), "madison");
        assert!(!key.session_id().is_empty());
        // UUID v4 format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
        assert!(key.session_id().contains('-'));
    }

    #[test]
    fn test_clone() {
        let key = SessionKey::new("riverside", "s-42");
        let cloned = key.clone();
        assert_eq!(key, cloned);
        assert_eq!(key.name(), cloned.name());
    }

    #[test]
    fn test_debug_format() {
        let key = SessionKey::new("amsterdam", "s-99");
        let debug = format!("{:?}", key);
        assert!(debug.contains("amsterdam"));
        assert!(debug.contains("s-99"));
    }
}
