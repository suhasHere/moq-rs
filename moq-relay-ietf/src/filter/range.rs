//! Compact range set implementation for efficient range-based filtering.
//!
//! Uses SmallVec to store ranges inline for common cases (1-4 ranges),
//! avoiding heap allocation in the hot path.

use smallvec::SmallVec;
use std::cmp::Ordering;

/// A set of non-overlapping, sorted ranges.
///
/// Optimized for the common case of 1-4 ranges, storing them inline.
/// Uses binary search for O(log n) containment checks.
///
/// # Memory Layout
/// - Inline: up to 4 ranges = 64 bytes (no heap allocation)
/// - Heap: >4 ranges, SmallVec spills to heap
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RangeSet {
    /// Sorted, non-overlapping ranges stored inline for <= 4 ranges.
    /// Each tuple is (start, end) inclusive.
    ranges: SmallVec<[(u64, u64); 4]>,
}

impl RangeSet {
    /// Creates an empty range set.
    #[inline]
    pub const fn new() -> Self {
        Self {
            ranges: SmallVec::new_const(),
        }
    }

    /// Creates a range set from a single range.
    #[inline]
    pub fn single(start: u64, end: u64) -> Self {
        let mut set = Self::new();
        set.insert(start, end);
        set
    }

    /// Creates a range set from an iterator of (start, end) tuples.
    pub fn from_ranges<I: IntoIterator<Item = (u64, u64)>>(iter: I) -> Self {
        let mut set = Self::new();
        for (start, end) in iter {
            set.insert(start, end);
        }
        set
    }

    /// Inserts a range into the set, merging overlapping ranges.
    pub fn insert(&mut self, start: u64, end: u64) {
        debug_assert!(start <= end, "invalid range: start > end");

        if self.ranges.is_empty() {
            self.ranges.push((start, end));
            return;
        }

        // Find insertion point
        let pos = self
            .ranges
            .binary_search_by(|(s, _)| s.cmp(&start))
            .unwrap_or_else(|p| p);

        // Check if we can merge with previous range
        if pos > 0 {
            let prev = &mut self.ranges[pos - 1];
            if prev.1 >= start.saturating_sub(1) {
                // Merge with previous
                prev.1 = prev.1.max(end);
                // Try to merge with subsequent ranges
                self.merge_forward(pos - 1);
                return;
            }
        }

        // Check if we can merge with next range
        if pos < self.ranges.len() {
            let next = &self.ranges[pos];
            if end >= next.0.saturating_sub(1) {
                // Merge with next
                self.ranges[pos] = (start, next.1.max(end));
                self.merge_forward(pos);
                return;
            }
        }

        // Insert new range
        self.ranges.insert(pos, (start, end));
    }

    /// Merges overlapping ranges starting from the given index.
    fn merge_forward(&mut self, idx: usize) {
        while idx + 1 < self.ranges.len() {
            let current_end = self.ranges[idx].1;
            let next_start = self.ranges[idx + 1].0;

            if current_end >= next_start.saturating_sub(1) {
                // Merge
                self.ranges[idx].1 = self.ranges[idx].1.max(self.ranges[idx + 1].1);
                self.ranges.remove(idx + 1);
            } else {
                break;
            }
        }
    }

    /// Checks if a value is contained in any range.
    ///
    /// Uses binary search for O(log n) lookup.
    #[inline]
    pub fn contains(&self, value: u64) -> bool {
        self.ranges
            .binary_search_by(|(start, end)| {
                if value < *start {
                    Ordering::Greater
                } else if value > *end {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Returns true if the range set is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Returns the number of ranges.
    #[inline]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Returns an iterator over the ranges.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &(u64, u64)> {
        self.ranges.iter()
    }

    /// Returns the total number of values covered by all ranges.
    pub fn coverage(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e - s + 1).sum()
    }

    /// Returns the span (max - min) of the range set.
    pub fn span(&self) -> Option<u64> {
        if self.ranges.is_empty() {
            None
        } else {
            let min = self.ranges.first().unwrap().0;
            let max = self.ranges.last().unwrap().1;
            Some(max - min)
        }
    }

    /// Returns true if this range set has dense coverage (>50% of span).
    pub fn is_dense(&self) -> bool {
        match self.span() {
            Some(span) if span > 0 => self.coverage() * 2 > span,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty() {
        let set = RangeSet::new();
        assert!(set.is_empty());
        assert!(!set.contains(0));
        assert!(!set.contains(100));
    }

    #[test]
    fn test_single_range() {
        let set = RangeSet::single(10, 20);
        assert!(!set.contains(9));
        assert!(set.contains(10));
        assert!(set.contains(15));
        assert!(set.contains(20));
        assert!(!set.contains(21));
    }

    #[test]
    fn test_multiple_ranges() {
        let set = RangeSet::from_ranges([(1, 5), (10, 15), (20, 25)]);
        assert_eq!(set.len(), 3);

        assert!(!set.contains(0));
        assert!(set.contains(1));
        assert!(set.contains(3));
        assert!(set.contains(5));
        assert!(!set.contains(6));
        assert!(!set.contains(9));
        assert!(set.contains(10));
        assert!(set.contains(12));
        assert!(set.contains(15));
        assert!(!set.contains(16));
        assert!(!set.contains(19));
        assert!(set.contains(20));
        assert!(set.contains(25));
        assert!(!set.contains(26));
    }

    #[test]
    fn test_merge_adjacent() {
        let mut set = RangeSet::new();
        set.insert(1, 5);
        set.insert(6, 10);
        assert_eq!(set.len(), 1);
        assert!(set.contains(1));
        assert!(set.contains(5));
        assert!(set.contains(6));
        assert!(set.contains(10));
    }

    #[test]
    fn test_merge_overlapping() {
        let mut set = RangeSet::new();
        set.insert(1, 10);
        set.insert(5, 15);
        assert_eq!(set.len(), 1);
        assert!(set.contains(1));
        assert!(set.contains(10));
        assert!(set.contains(15));
    }

    #[test]
    fn test_coverage() {
        let set = RangeSet::from_ranges([(1, 5), (10, 15)]);
        // (5-1+1) + (15-10+1) = 5 + 6 = 11
        assert_eq!(set.coverage(), 11);
    }

    #[test]
    fn test_is_dense() {
        // Dense: 90% coverage
        let dense = RangeSet::from_ranges([(1, 90)]);
        assert!(dense.is_dense());

        // Sparse: 20% coverage over span of 100
        let sparse = RangeSet::from_ranges([(1, 10), (91, 100)]);
        assert!(!sparse.is_dense());
    }
}
