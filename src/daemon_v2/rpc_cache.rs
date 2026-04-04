//! RPC response cache — caches read-only RPC responses for a short TTL
//! to reduce redundant computation during frequent web UI polling.
//!
//! Section 15 Nice to Have: RPC response caching

use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Cached RPC response with expiration time.
struct CacheEntry {
    response: Value,
    expires_at: Instant,
}

/// Simple TTL cache for RPC responses.
pub struct RpcCache {
    entries: HashMap<String, CacheEntry>,
    ttl: Duration,
}

/// Methods that are safe to cache (read-only, no side effects).
const CACHEABLE_METHODS: &[&str] = &[
    "status",
    "agent.list",
    "task.list",
    "pr.list",
    "channel.list",
    "ping",
    "version",
];

impl RpcCache {
    /// Create a new cache with the given TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            ttl,
        }
    }

    /// Check if a method is cacheable.
    pub fn is_cacheable(method: &str) -> bool {
        CACHEABLE_METHODS.contains(&method)
    }

    /// Get a cached response if it exists and hasn't expired.
    pub fn get(&self, key: &str) -> Option<&Value> {
        let entry = self.entries.get(key)?;
        if Instant::now() < entry.expires_at {
            Some(&entry.response)
        } else {
            None
        }
    }

    /// Store a response in the cache.
    pub fn set(&mut self, key: String, response: Value) {
        self.entries.insert(
            key,
            CacheEntry {
                response,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    /// Invalidate all cached entries (called after mutating operations).
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// RPC cache returns cached value within TTL
    #[test]
    fn cache_hit_within_ttl() {
        let mut cache = RpcCache::new(Duration::from_secs(60));
        cache.set("status".into(), json!({"agents": {"total": 5}}));
        assert!(cache.get("status").is_some());
        assert_eq!(cache.get("status").unwrap()["agents"]["total"], 5);
    }

    /// RPC cache returns None for expired entries
    #[test]
    fn cache_miss_after_expiry() {
        let mut cache = RpcCache::new(Duration::from_millis(1));
        cache.set("status".into(), json!({"stale": true}));
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("status").is_none());
    }

    /// invalidate_all clears all entries
    #[test]
    fn invalidate_clears_cache() {
        let mut cache = RpcCache::new(Duration::from_secs(60));
        cache.set("status".into(), json!({"cached": true}));
        cache.set("task.list".into(), json!([]));
        assert!(cache.get("status").is_some());
        cache.invalidate_all();
        assert!(cache.get("status").is_none());
        assert!(cache.get("task.list").is_none());
    }

    /// Only read-only methods are cacheable
    #[test]
    fn cacheable_methods() {
        assert!(RpcCache::is_cacheable("status"));
        assert!(RpcCache::is_cacheable("agent.list"));
        assert!(RpcCache::is_cacheable("ping"));
        assert!(!RpcCache::is_cacheable("task.create"));
        assert!(!RpcCache::is_cacheable("channel.post"));
        assert!(!RpcCache::is_cacheable("shutdown"));
    }
}
