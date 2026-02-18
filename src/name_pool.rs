use std::collections::{HashSet, VecDeque};

#[path = "name_pool_tests.rs"]
#[cfg(test)]
mod tests;

/// LRU name pool: names at the front are least-recently-used.
/// Allocated names are removed from the queue; released names go to the back.
#[derive(Debug, Clone)]
pub struct NamePool {
    /// Available names in LRU order (front = least recently used).
    available: VecDeque<String>,
    /// Currently allocated names.
    allocated: HashSet<String>,
    /// All names in the pool (for validation and restore).
    all_names: Vec<String>,
}

impl NamePool {
    pub fn new(names: &[&str]) -> Self {
        let all_names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        Self {
            available: all_names.iter().cloned().collect(),
            allocated: HashSet::new(),
            all_names,
        }
    }

    /// Allocate a name. If `preferred` is available, use it. Otherwise take LRU.
    pub fn allocate(&mut self, preferred: Option<&str>) -> Option<String> {
        // Try preferred name first
        if let Some(pref) = preferred
            && let Some(pos) = self
                .available
                .iter()
                .position(|n| n.eq_ignore_ascii_case(pref))
        {
            let name = self.available.remove(pos).unwrap();
            self.allocated.insert(name.clone());
            return Some(name);
        }
        // Fall back to LRU (front of queue)
        if let Some(name) = self.available.pop_front() {
            self.allocated.insert(name.clone());
            Some(name)
        } else {
            None
        }
    }

    /// Allocate, skipping names in the exclusion set.
    pub fn allocate_excluding(
        &mut self,
        preferred: Option<&str>,
        excluded: &HashSet<String>,
    ) -> Option<String> {
        // Try preferred if not excluded
        if let Some(pref) = preferred
            && !excluded.contains(&pref.to_lowercase())
            && let Some(pos) = self
                .available
                .iter()
                .position(|n| n.eq_ignore_ascii_case(pref))
        {
            let name = self.available.remove(pos).unwrap();
            self.allocated.insert(name.clone());
            return Some(name);
        }
        // Find first non-excluded LRU name
        let pos = self
            .available
            .iter()
            .position(|n| !excluded.contains(&n.to_lowercase()))?;
        let name = self.available.remove(pos).unwrap();
        self.allocated.insert(name.clone());
        Some(name)
    }

    /// Release a name back to the pool (goes to back = most recently used).
    /// Idempotent: releasing a name not in the allocated set is a no-op.
    pub fn release(&mut self, name: &str) {
        // Try exact match first, then case-insensitive
        let removed = self.allocated.remove(name) || {
            // Find the canonical casing in allocated set
            if let Some(canonical) = self
                .allocated
                .iter()
                .find(|n| n.eq_ignore_ascii_case(name))
                .cloned()
            {
                self.allocated.remove(&canonical)
            } else {
                false
            }
        };

        if removed {
            // Find canonical form from all_names
            let canonical = self
                .all_names
                .iter()
                .find(|n| n.eq_ignore_ascii_case(name))
                .cloned()
                .unwrap_or_else(|| name.to_string());
            if !self.available.contains(&canonical) {
                self.available.push_back(canonical);
            }
        }
    }

    pub fn is_allocated(&self, name: &str) -> bool {
        self.allocated.contains(name) || self.allocated.iter().any(|n| n.eq_ignore_ascii_case(name))
    }

    pub fn available_count(&self) -> usize {
        self.available.len()
    }

    pub fn allocated_count(&self) -> usize {
        self.allocated.len()
    }

    /// Get the name currently at the front of the LRU queue (next to be allocated).
    pub fn peek_next(&self) -> Option<&str> {
        self.available.front().map(|s| s.as_str())
    }

    /// All names (allocated + available).
    pub fn all_names(&self) -> &[String] {
        &self.all_names
    }

    /// Restore pool state from persisted session data (daemon restart).
    /// Names with active sessions are marked allocated; the rest are available in LRU order.
    pub fn restore(&mut self, allocated_names: &[String]) {
        self.allocated.clear();
        self.available.clear();
        for name in &self.all_names {
            if allocated_names.iter().any(|a| a.eq_ignore_ascii_case(name)) {
                self.allocated.insert(name.clone());
            } else {
                self.available.push_back(name.clone());
            }
        }
    }
}
