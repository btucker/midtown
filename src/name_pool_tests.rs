use crate::name_pool::NamePool;

#[test]
fn test_allocate_returns_least_recently_used() {
    let mut pool = NamePool::new(&["a", "b", "c"]);
    assert_eq!(pool.allocate(None), Some("a".to_string()));
    assert_eq!(pool.allocate(None), Some("b".to_string()));
    assert_eq!(pool.allocate(None), Some("c".to_string()));
    assert_eq!(pool.allocate(None), None); // exhausted
}

#[test]
fn test_release_returns_name_to_back_of_queue() {
    let mut pool = NamePool::new(&["a", "b", "c"]);
    let name = pool.allocate(None).unwrap(); // "a"
    pool.release(&name);
    // "b" is now front (LRU), "a" went to back
    assert_eq!(pool.allocate(None), Some("b".to_string()));
    assert_eq!(pool.allocate(None), Some("c".to_string()));
    assert_eq!(pool.allocate(None), Some("a".to_string()));
}

#[test]
fn test_preferred_name_honored_when_available() {
    let mut pool = NamePool::new(&["a", "b", "c"]);
    pool.allocate(None); // takes "a"
    pool.allocate(None); // takes "b"
    pool.release("a"); // "a" returns to pool
    // Preferred name "a" is available — skip LRU order
    assert_eq!(pool.allocate(Some("a")), Some("a".to_string()));
}

#[test]
fn test_preferred_name_falls_back_to_lru_when_in_use() {
    let mut pool = NamePool::new(&["a", "b", "c"]);
    pool.allocate(None); // takes "a" (in use)
    // Preferred "a" is in use, fall back to LRU
    assert_eq!(pool.allocate(Some("a")), Some("b".to_string()));
}

#[test]
fn test_release_idempotent() {
    let mut pool = NamePool::new(&["a", "b"]);
    pool.allocate(None); // takes "a"
    pool.release("a");
    pool.release("a"); // second release is no-op
    assert_eq!(pool.available_count(), 2); // still just 2 total
}

#[test]
fn test_allocate_excluding() {
    let mut pool = NamePool::new(&["a", "b", "c"]);
    let excluded: std::collections::HashSet<String> = ["a"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        pool.allocate_excluding(None, &excluded),
        Some("b".to_string())
    );
}

#[test]
fn test_is_allocated() {
    let mut pool = NamePool::new(&["a", "b"]);
    assert!(!pool.is_allocated("a"));
    pool.allocate(None);
    assert!(pool.is_allocated("a"));
    pool.release("a");
    assert!(!pool.is_allocated("a"));
}
