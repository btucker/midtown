//! Test utilities shared across unit and integration tests.
//!
//! This module provides common test helpers to reduce duplication.
//! It's only compiled when tests are being built (cfg(test)).

use std::thread;
use std::time::Duration;

/// Retry a fallible operation with backoff to handle transient errors.
///
/// This is particularly useful for handling lock contention in tests:
/// - Channel read methods use `try_lock_shared()` which returns `WouldBlock`
///   when an exclusive write lock is held
/// - File operations may experience temporary lock contention on CI systems
///
/// The backoff strategy increases sleep duration linearly: 10ms, 20ms, 30ms, etc.
///
/// # Arguments
/// * `max_attempts` - Maximum number of retry attempts (must be > 0)
/// * `f` - The fallible operation to retry
///
/// # Returns
/// * `Ok(T)` - If the operation succeeds within max_attempts
/// * `Err(E)` - If all attempts fail, returns the last error
///
/// # Panics
/// Panics if `max_attempts` is 0 (no attempts would be made).
///
/// # Example
/// ```ignore
/// use midtown::test_utils::retry_with_backoff;
///
/// let result = retry_with_backoff(5, || {
///     // Operation that might fail transiently
///     channel.read_all()
/// });
/// ```
pub fn retry_with_backoff<T, E: std::fmt::Debug>(
    max_attempts: u32,
    mut f: impl FnMut() -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    assert!(max_attempts > 0, "max_attempts must be greater than 0");

    for attempt in 0..max_attempts {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) if attempt < max_attempts - 1 => {
                thread::sleep(Duration::from_millis(10 * (attempt as u64 + 1)));
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("loop should always return before reaching here")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn test_retry_succeeds_first_attempt() {
        let result = retry_with_backoff(5, || Ok::<i32, &str>(42));
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn test_retry_succeeds_after_failures() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = retry_with_backoff(5, move || {
            let count = counter_clone.fetch_add(1, Ordering::SeqCst);
            if count < 2 {
                Err("transient error")
            } else {
                Ok(42)
            }
        });

        assert_eq!(result, Ok(42));
        assert_eq!(counter.load(Ordering::SeqCst), 3); // Failed twice, succeeded on third
    }

    #[test]
    fn test_retry_fails_after_all_attempts() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = retry_with_backoff(3, move || {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            Err::<i32, &str>("persistent error")
        });

        assert_eq!(result, Err("persistent error"));
        assert_eq!(counter.load(Ordering::SeqCst), 3); // All attempts exhausted
    }

    #[test]
    #[should_panic(expected = "max_attempts must be greater than 0")]
    fn test_retry_panics_with_zero_attempts() {
        let _ = retry_with_backoff(0, || Ok::<i32, &str>(42));
    }

    #[test]
    fn test_retry_backoff_timing() {
        use std::time::Instant;

        let start = Instant::now();
        let _result = retry_with_backoff(3, || {
            Err::<i32, &str>("error") // Force all retries
        });
        let elapsed = start.elapsed();

        // With 3 attempts: sleep 10ms after 1st attempt, 20ms after 2nd attempt
        // Total sleep should be ~30ms (allow some margin for scheduling)
        assert!(
            elapsed >= Duration::from_millis(30),
            "Expected at least 30ms of backoff, got {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(100),
            "Backoff took too long: {:?}",
            elapsed
        );
    }
}
