//! Track Extension Filter implementation.
//!
//! Filters based on track extension metadata before other filter stages.
//! Uses a hash map for O(1) extension type lookup and RangeSet for range matching.

use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{FilterStats, RangeSet};

/// Extension type identifier (as defined in MoQ spec).
pub type ExtensionType = u64;

/// Extension value type.
pub type ExtensionValue = u64;

/// A matcher for a specific extension type's values.
#[derive(Clone, Debug)]
pub struct ExtensionMatcher {
    /// The extension type this matcher handles.
    pub extension_type: ExtensionType,

    /// Allowed value ranges for this extension.
    pub allowed_ranges: RangeSet,
}

impl ExtensionMatcher {
    /// Creates a new extension matcher.
    pub fn new(extension_type: ExtensionType) -> Self {
        Self {
            extension_type,
            allowed_ranges: RangeSet::new(),
        }
    }

    /// Creates a matcher that allows a single value.
    pub fn exact(extension_type: ExtensionType, value: ExtensionValue) -> Self {
        Self {
            extension_type,
            allowed_ranges: RangeSet::single(value, value),
        }
    }

    /// Creates a matcher that allows a range of values.
    pub fn range(extension_type: ExtensionType, start: ExtensionValue, end: ExtensionValue) -> Self {
        Self {
            extension_type,
            allowed_ranges: RangeSet::single(start, end),
        }
    }

    /// Adds an allowed value range.
    pub fn allow_range(&mut self, start: ExtensionValue, end: ExtensionValue) {
        self.allowed_ranges.insert(start, end);
    }

    /// Checks if a value matches this extension filter.
    #[inline]
    pub fn matches(&self, value: ExtensionValue) -> bool {
        self.allowed_ranges.contains(value)
    }
}

/// Track Extension Filter for filtering based on track extension metadata.
///
/// This is the first stage of the filter pipeline.
///
/// # Performance
/// - Extension type lookup: O(1) using FxHashMap
/// - Value range check: O(log r) where r is number of ranges
/// - Memory: ~64 bytes per extension type + 16 bytes per range
pub struct TrackExtensionFilter {
    /// Extension matchers indexed by extension type.
    /// Uses FxHashMap for faster hashing than std HashMap.
    matchers: FxHashMap<ExtensionType, ExtensionMatcher>,

    /// Whether this filter is enabled.
    enabled: AtomicBool,

    /// Statistics collector.
    pub stats: FilterStats,
}

impl TrackExtensionFilter {
    /// Creates a new empty track extension filter.
    pub fn new() -> Self {
        Self {
            matchers: FxHashMap::default(),
            enabled: AtomicBool::new(true),
            stats: FilterStats::new(),
        }
    }

    /// Creates a filter with statistics enabled.
    pub fn with_stats() -> Self {
        Self {
            matchers: FxHashMap::default(),
            enabled: AtomicBool::new(true),
            stats: FilterStats::enabled(),
        }
    }

    /// Enables this filter stage.
    #[inline]
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Disables this filter stage.
    #[inline]
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// Returns true if this filter is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Adds an extension matcher to the filter.
    pub fn add_matcher(&mut self, matcher: ExtensionMatcher) {
        self.matchers.insert(matcher.extension_type, matcher);
    }

    /// Adds a filter for a specific extension type with a single value.
    pub fn filter_exact(&mut self, extension_type: ExtensionType, value: ExtensionValue) {
        let matcher = ExtensionMatcher::exact(extension_type, value);
        self.matchers.insert(extension_type, matcher);
    }

    /// Adds a filter for a specific extension type with a range of values.
    pub fn filter_range(
        &mut self,
        extension_type: ExtensionType,
        start: ExtensionValue,
        end: ExtensionValue,
    ) {
        match self.matchers.get_mut(&extension_type) {
            Some(matcher) => {
                matcher.allow_range(start, end);
            }
            None => {
                let matcher = ExtensionMatcher::range(extension_type, start, end);
                self.matchers.insert(extension_type, matcher);
            }
        }
    }

    /// Checks if the given extensions pass the filter.
    ///
    /// Returns true if:
    /// - The filter is disabled, OR
    /// - No matchers are configured, OR
    /// - All configured extension types have values within allowed ranges
    ///
    /// Note: Extensions not configured in the filter are ignored (pass through).
    #[inline]
    pub fn matches(&self, extensions: &[(ExtensionType, ExtensionValue)]) -> bool {
        if !self.is_enabled() || self.matchers.is_empty() {
            return true;
        }

        for (ext_type, value) in extensions {
            if let Some(matcher) = self.matchers.get(ext_type) {
                if !matcher.matches(*value) {
                    return false;
                }
            }
        }

        true
    }

    /// Checks extensions and records statistics.
    #[inline]
    pub fn filter(&self, extensions: &[(ExtensionType, ExtensionValue)]) -> bool {
        let result = self.matches(extensions);
        self.stats.record(result);
        result
    }

    /// Returns the number of configured extension matchers.
    #[inline]
    pub fn matcher_count(&self) -> usize {
        self.matchers.len()
    }

    /// Estimates memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let matchers: usize = self
            .matchers
            .iter()
            .map(|(_, m)| {
                std::mem::size_of::<ExtensionMatcher>()
                    + m.allowed_ranges.len() * std::mem::size_of::<(u64, u64)>()
            })
            .sum();
        base + matchers
    }
}

impl Default for TrackExtensionFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_filter() {
        let filter = TrackExtensionFilter::new();
        assert!(filter.matches(&[]));
        assert!(filter.matches(&[(1, 100), (2, 200)]));
    }

    #[test]
    fn test_exact_match() {
        let mut filter = TrackExtensionFilter::new();
        filter.filter_exact(1, 100);

        assert!(filter.matches(&[(1, 100)]));
        assert!(!filter.matches(&[(1, 101)]));
        assert!(filter.matches(&[(1, 100), (2, 200)])); // Extension 2 not filtered
    }

    #[test]
    fn test_range_match() {
        let mut filter = TrackExtensionFilter::new();
        filter.filter_range(1, 10, 20);

        assert!(!filter.matches(&[(1, 9)]));
        assert!(filter.matches(&[(1, 10)]));
        assert!(filter.matches(&[(1, 15)]));
        assert!(filter.matches(&[(1, 20)]));
        assert!(!filter.matches(&[(1, 21)]));
    }

    #[test]
    fn test_multiple_matchers() {
        let mut filter = TrackExtensionFilter::new();
        filter.filter_range(1, 10, 20);
        filter.filter_exact(2, 100);

        // Both must match
        assert!(filter.matches(&[(1, 15), (2, 100)]));
        assert!(!filter.matches(&[(1, 15), (2, 101)]));
        assert!(!filter.matches(&[(1, 25), (2, 100)]));
    }

    #[test]
    fn test_disabled_filter() {
        let mut filter = TrackExtensionFilter::new();
        filter.filter_exact(1, 100);
        filter.disable();

        // Should pass everything when disabled
        assert!(filter.matches(&[(1, 999)]));
    }

    #[test]
    fn test_stats_collection() {
        let mut filter = TrackExtensionFilter::with_stats();
        filter.filter_exact(1, 100);

        filter.filter(&[(1, 100)]); // Pass
        filter.filter(&[(1, 100)]); // Pass
        filter.filter(&[(1, 101)]); // Fail

        let stats = filter.stats.snapshot();
        assert_eq!(stats.passed, 2);
        assert_eq!(stats.filtered, 1);
    }
}
