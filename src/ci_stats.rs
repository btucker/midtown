//! CI check duration statistics for auto-retry of stale checks.
//!
//! Tracks historical completion times for CI checks to detect when checks
//! are running significantly longer than typical (4x threshold). When a check
//! exceeds this threshold, the daemon can trigger a re-run.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

/// Maximum number of duration samples to keep per check name.
const MAX_SAMPLES_PER_CHECK: usize = 20;

/// Minimum number of samples needed before we can calculate a reliable typical duration.
const MIN_SAMPLES_FOR_THRESHOLD: usize = 3;

/// Default typical duration (in seconds) when we don't have enough samples.
/// Conservative default of 10 minutes.
const DEFAULT_TYPICAL_DURATION_SECS: u64 = 600;

/// Cooldown period before re-running the same workflow again (1 hour).
pub const RERUN_COOLDOWN_SECS: i64 = 3600;

/// Multiplier for detecting stale checks. A check is considered stale if it's
/// been running for more than this multiple of the typical duration.
pub const STALE_THRESHOLD_MULTIPLIER: f64 = 4.0;

/// Statistics for CI check durations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CiCheckStats {
    /// Rolling window of recent durations (in seconds) per check name.
    /// Key is the check name (e.g., "Test", "Clippy", "E2E - idle_break_e2e").
    #[serde(default)]
    pub check_durations: HashMap<String, Vec<u64>>,

    /// Cooldown tracking for workflow re-runs.
    /// Key is workflow run ID, value is when we last triggered a re-run.
    /// This prevents re-running the same workflow multiple times.
    #[serde(default)]
    pub rerun_cooldowns: HashMap<u64, DateTime<Utc>>,
}

impl CiCheckStats {
    /// Record a check completion duration.
    ///
    /// Call this when a check_run webhook reports completion, passing the
    /// duration calculated from `completed_at - started_at`.
    pub fn record_duration(&mut self, check_name: &str, duration_secs: u64) {
        let samples = self
            .check_durations
            .entry(check_name.to_string())
            .or_default();

        samples.push(duration_secs);

        // Keep only the most recent samples
        if samples.len() > MAX_SAMPLES_PER_CHECK {
            samples.remove(0);
        }

        debug!(
            "Recorded CI duration: {} took {}s (now have {} samples)",
            check_name,
            duration_secs,
            samples.len()
        );
    }

    /// Get the typical (median) duration for a check in seconds.
    ///
    /// Returns `None` if we don't have enough samples yet.
    pub fn typical_duration(&self, check_name: &str) -> Option<u64> {
        let samples = self.check_durations.get(check_name)?;

        if samples.len() < MIN_SAMPLES_FOR_THRESHOLD {
            return None;
        }

        // Calculate median
        let mut sorted = samples.clone();
        sorted.sort_unstable();
        let mid = sorted.len() / 2;

        Some(if sorted.len() % 2 == 0 {
            (sorted[mid - 1] + sorted[mid]) / 2
        } else {
            sorted[mid]
        })
    }

    /// Get the typical duration, falling back to default if not enough samples.
    pub fn typical_duration_or_default(&self, check_name: &str) -> u64 {
        self.typical_duration(check_name)
            .unwrap_or(DEFAULT_TYPICAL_DURATION_SECS)
    }

    /// Check if a running duration exceeds the stale threshold for this check.
    ///
    /// Returns `true` if the check has been running for more than 4x the typical duration.
    pub fn is_stale(&self, check_name: &str, running_duration_secs: u64) -> bool {
        let typical = self.typical_duration_or_default(check_name);
        let threshold = (typical as f64 * STALE_THRESHOLD_MULTIPLIER) as u64;
        running_duration_secs > threshold
    }

    /// Check if we can re-run a workflow (not on cooldown).
    pub fn can_rerun(&self, run_id: u64) -> bool {
        match self.rerun_cooldowns.get(&run_id) {
            Some(last_rerun) => {
                let elapsed = Utc::now().signed_duration_since(*last_rerun);
                elapsed > chrono::Duration::seconds(RERUN_COOLDOWN_SECS)
            }
            None => true,
        }
    }

    /// Record that we triggered a re-run for a workflow.
    pub fn record_rerun(&mut self, run_id: u64) {
        self.rerun_cooldowns.insert(run_id, Utc::now());
    }

    /// Clean up stale cooldown entries (older than 24 hours).
    pub fn cleanup_stale_cooldowns(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::hours(24);
        self.rerun_cooldowns.retain(|_, ts| *ts > cutoff);
    }

    /// Get statistics summary for logging/debugging.
    pub fn summary(&self) -> String {
        let check_count = self.check_durations.len();
        let total_samples: usize = self.check_durations.values().map(|v| v.len()).sum();
        format!(
            "{} checks tracked, {} total samples",
            check_count, total_samples
        )
    }
}

/// Information about a stale CI check detected during PR polling.
#[derive(Debug, Clone)]
pub struct StaleCheck {
    /// PR number
    pub pr_number: u64,
    /// Check name (e.g., "Test")
    pub check_name: String,
    /// Workflow run ID (extracted from detailsUrl)
    pub run_id: u64,
    /// How long the check has been running (seconds)
    pub running_duration_secs: u64,
    /// Typical duration for this check (seconds)
    pub typical_duration_secs: u64,
}

/// Extract workflow run ID from a GitHub Actions details URL.
///
/// Example URL: `https://github.com/owner/repo/actions/runs/21672778453/job/62484733462`
/// Returns: `Some(21672778453)`
pub fn extract_run_id_from_url(url: &str) -> Option<u64> {
    // Look for /runs/<number>/ pattern
    let runs_idx = url.find("/runs/")?;
    let after_runs = &url[runs_idx + 6..];
    let end_idx = after_runs.find('/').unwrap_or(after_runs.len());
    after_runs[..end_idx].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_duration() {
        let mut stats = CiCheckStats::default();
        stats.record_duration("Test", 120);
        stats.record_duration("Test", 130);
        stats.record_duration("Test", 125);

        assert_eq!(stats.check_durations.get("Test").unwrap().len(), 3);
    }

    #[test]
    fn test_typical_duration_median() {
        let mut stats = CiCheckStats::default();
        // Add 5 samples: 100, 110, 120, 130, 140
        for duration in [100, 110, 120, 130, 140] {
            stats.record_duration("Test", duration);
        }

        // Median of [100, 110, 120, 130, 140] is 120
        assert_eq!(stats.typical_duration("Test"), Some(120));
    }

    #[test]
    fn test_typical_duration_not_enough_samples() {
        let mut stats = CiCheckStats::default();
        stats.record_duration("Test", 120);
        stats.record_duration("Test", 130);

        // Only 2 samples, need at least 3
        assert_eq!(stats.typical_duration("Test"), None);
    }

    #[test]
    fn test_is_stale() {
        let mut stats = CiCheckStats::default();
        // Typical duration: 120 seconds
        for _ in 0..5 {
            stats.record_duration("Test", 120);
        }

        // 4x threshold = 480 seconds
        assert!(!stats.is_stale("Test", 400)); // Below threshold
        assert!(!stats.is_stale("Test", 480)); // At threshold (not stale, need to exceed)
        assert!(stats.is_stale("Test", 481)); // Above threshold
        assert!(stats.is_stale("Test", 600)); // Well above threshold
    }

    #[test]
    fn test_is_stale_default_threshold() {
        let stats = CiCheckStats::default();

        // No samples, uses default of 600s, 4x = 2400s
        assert!(!stats.is_stale("Unknown", 2000));
        assert!(stats.is_stale("Unknown", 2500));
    }

    #[test]
    fn test_rerun_cooldown() {
        let mut stats = CiCheckStats::default();

        // First rerun should be allowed
        assert!(stats.can_rerun(12345));

        // Record rerun
        stats.record_rerun(12345);

        // Immediate second rerun should be blocked
        assert!(!stats.can_rerun(12345));

        // Different run ID should be allowed
        assert!(stats.can_rerun(67890));
    }

    #[test]
    fn test_max_samples_limit() {
        let mut stats = CiCheckStats::default();

        // Add more than MAX_SAMPLES_PER_CHECK samples
        for i in 0..30 {
            stats.record_duration("Test", i * 10);
        }

        // Should be capped at MAX_SAMPLES_PER_CHECK
        assert_eq!(
            stats.check_durations.get("Test").unwrap().len(),
            MAX_SAMPLES_PER_CHECK
        );

        // Oldest samples should be removed (FIFO)
        let samples = stats.check_durations.get("Test").unwrap();
        assert_eq!(samples[0], 100); // 10th sample (indices 10-29 remain)
    }

    #[test]
    fn test_extract_run_id_from_url() {
        let url = "https://github.com/btucker/midtown/actions/runs/21672778453/job/62484733462";
        assert_eq!(extract_run_id_from_url(url), Some(21672778453));

        let url2 = "https://github.com/owner/repo/actions/runs/12345";
        assert_eq!(extract_run_id_from_url(url2), Some(12345));

        let invalid = "https://github.com/owner/repo/pull/42";
        assert_eq!(extract_run_id_from_url(invalid), None);
    }

    #[test]
    fn test_cleanup_stale_cooldowns() {
        let mut stats = CiCheckStats::default();

        // Add a recent cooldown
        stats.record_rerun(111);

        // Add a stale cooldown (manually backdate)
        stats
            .rerun_cooldowns
            .insert(222, Utc::now() - chrono::Duration::hours(25));

        stats.cleanup_stale_cooldowns();

        assert!(stats.rerun_cooldowns.contains_key(&111));
        assert!(!stats.rerun_cooldowns.contains_key(&222));
    }
}
