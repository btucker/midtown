//! Generic TTL cache for daemon RPC handlers.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Thread-safe TTL cache keyed by a `u64` hash.
///
/// Stores a single value of type `V` together with the timestamp it was
/// inserted and the cache key it was inserted under. A hit requires both that
/// the entry is younger than `ttl` **and** that the stored key matches the
/// requested key.
pub(crate) struct KeyedValueCache<V: Clone> {
    inner: Mutex<Option<(Instant, V, u64)>>,
    ttl: Duration,
}

impl<V: Clone> KeyedValueCache<V> {
    pub(crate) fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(None),
            ttl,
        }
    }

    /// Return the cached value if it exists, is younger than the TTL, and
    /// matches `cache_key`.
    pub(crate) fn get(&self, cache_key: u64) -> Option<V> {
        let guard = self.inner.lock().ok()?;
        guard
            .as_ref()
            .filter(|(ts, _, key)| ts.elapsed() < self.ttl && *key == cache_key)
            .map(|(_, v, _)| v.clone())
    }

    /// Store a new value with the current timestamp and `cache_key`.
    pub(crate) fn set(&self, value: V, cache_key: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some((Instant::now(), value, cache_key));
        }
    }

    /// Remove the cached entry if it has exceeded the TTL.
    pub(crate) fn cleanup(&self) {
        if let Ok(mut guard) = self.inner.lock()
            && guard
                .as_ref()
                .is_some_and(|(ts, _, _)| ts.elapsed() >= self.ttl)
        {
            *guard = None;
        }
    }
}
