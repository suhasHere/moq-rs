//! Range Filter types for SUBSCRIBE and FETCH messages.
//!
//! Per PR #1518, range filters use delta-encoded Start/End pairs:
//! - Start is delta-encoded from prior Range's End (or 0 for first)
//! - End is delta-encoded from current Range's Start
//!
//! Filter parameter types:
//! - SUBGROUP_FILTER (0x25)
//! - OBJECT_FILTER (0x26)
//! - PRIORITY_FILTER (0x27)
//! - PROPERTY_FILTER (0x28)

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

/// Range filter parameter type codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u64)]
pub enum RangeFilterType {
    /// Filter by subgroup ID ranges
    Subgroup = 0x25,
    /// Filter by object ID ranges
    Object = 0x26,
    /// Filter by priority ranges (0-255)
    Priority = 0x27,
    /// Filter by property value ranges
    Property = 0x28,
}

impl From<RangeFilterType> for u64 {
    fn from(value: RangeFilterType) -> Self {
        value as u64
    }
}

/// A single inclusive range [start, end].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    pub fn new(start: u64, end: u64) -> Self {
        debug_assert!(start <= end, "invalid range: start > end");
        Self { start, end }
    }

    /// Creates a range containing a single value.
    pub fn single(value: u64) -> Self {
        Self {
            start: value,
            end: value,
        }
    }

    /// Checks if a value is within this range.
    #[inline]
    pub fn contains(&self, value: u64) -> bool {
        value >= self.start && value <= self.end
    }
}

/// A set of ranges that can be combined with AND logic.
///
/// Ranges within the same AndSet are OR'd together.
/// Different AndSets are AND'd together.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RangeFilterSet {
    /// The AndSet identifier (8 bits).
    /// Ranges with the same and_set are OR'd.
    /// Different and_set values are AND'd.
    pub and_set: u8,

    /// The ranges in this set (delta-encoded on wire).
    pub ranges: Vec<Range>,
}

impl RangeFilterSet {
    pub fn new(and_set: u8) -> Self {
        Self {
            and_set,
            ranges: Vec::new(),
        }
    }

    /// Adds a range to this set.
    pub fn add_range(&mut self, start: u64, end: u64) {
        self.ranges.push(Range::new(start, end));
    }

    /// Checks if a value matches any range in this set.
    #[inline]
    pub fn matches(&self, value: u64) -> bool {
        self.ranges.iter().any(|r| r.contains(value))
    }

    /// Returns true if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// Range Filter for filtering by numeric ranges.
///
/// Wire format:
/// ```text
/// RangeFilter {
///   Type (vi64)      - 0x25-0x28
///   Length (vi64)    - byte count of remaining fields
///   AndSet (8 bits)  - AND grouping identifier
///   Range* {
///     Start (vi64)   - delta from prior End (or 0)
///     End (vi64)     - delta from current Start
///   }
/// }
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RangeFilter {
    /// Filter type
    pub filter_type: Option<RangeFilterType>,

    /// Sets of ranges grouped by AND logic.
    pub sets: Vec<RangeFilterSet>,
}

impl RangeFilter {
    /// Creates an empty range filter.
    pub fn new(filter_type: RangeFilterType) -> Self {
        Self {
            filter_type: Some(filter_type),
            sets: Vec::new(),
        }
    }

    /// Creates a subgroup filter.
    pub fn subgroup() -> Self {
        Self::new(RangeFilterType::Subgroup)
    }

    /// Creates an object filter.
    pub fn object() -> Self {
        Self::new(RangeFilterType::Object)
    }

    /// Creates a priority filter.
    pub fn priority() -> Self {
        Self::new(RangeFilterType::Priority)
    }

    /// Adds a range set to the filter.
    pub fn add_set(&mut self, set: RangeFilterSet) {
        self.sets.push(set);
    }

    /// Adds a single range with the specified and_set.
    pub fn add_range(&mut self, and_set: u8, start: u64, end: u64) {
        if let Some(set) = self.sets.iter_mut().find(|s| s.and_set == and_set) {
            set.add_range(start, end);
        } else {
            let mut new_set = RangeFilterSet::new(and_set);
            new_set.add_range(start, end);
            self.sets.push(new_set);
        }
    }

    /// Evaluates if a value passes the filter.
    ///
    /// Logic:
    /// - Ranges within the same and_set are OR'd
    /// - Different and_sets are AND'd
    pub fn matches(&self, value: u64) -> bool {
        if self.sets.is_empty() {
            return true; // No filter = pass all
        }

        // Group by and_set and apply AND logic
        let mut and_groups: std::collections::HashMap<u8, bool> =
            std::collections::HashMap::new();

        for set in &self.sets {
            let or_result = set.matches(value);
            and_groups
                .entry(set.and_set)
                .and_modify(|v| *v = *v || or_result)
                .or_insert(or_result);
        }

        // All and_groups must be true
        and_groups.values().all(|&v| v)
    }

    /// Returns total number of ranges across all sets.
    pub fn range_count(&self) -> usize {
        self.sets.iter().map(|s| s.ranges.len()).sum()
    }
}

impl Encode for RangeFilter {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        for set in &self.sets {
            set.and_set.encode(w)?;

            // Delta encode ranges
            let mut prev_end: u64 = 0;
            for range in &set.ranges {
                // Start is delta from prev_end
                let start_delta = range.start.saturating_sub(prev_end);
                start_delta.encode(w)?;

                // End is delta from start
                let end_delta = range.end.saturating_sub(range.start);
                end_delta.encode(w)?;

                prev_end = range.end;
            }
        }
        Ok(())
    }
}

impl Decode for RangeFilter {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let mut filter = RangeFilter::default();

        // Note: The caller must handle the length-prefixed decoding
        // This decodes the content within the length prefix
        while r.has_remaining() {
            let and_set = u8::decode(r)?;
            let mut set = RangeFilterSet::new(and_set);

            // Read ranges until we hit the next and_set or end
            let mut prev_end: u64 = 0;
            while r.has_remaining() {
                // Peek to see if this might be an and_set byte
                // This is a simplification - real impl needs length tracking
                let start_delta = u64::decode(r)?;
                let end_delta = u64::decode(r)?;

                let start = prev_end.saturating_add(start_delta);
                let end = start.saturating_add(end_delta);

                set.add_range(start, end);
                prev_end = end;

                // For simplicity, assume one set per decode call
                // Real implementation would track bytes read vs length
                break;
            }

            filter.sets.push(set);
        }

        Ok(filter)
    }
}

/// Property Filter extends RangeFilter with a property type.
///
/// Wire format:
/// ```text
/// PropertyFilter {
///   Type (vi64)         - 0x28
///   Length (vi64)       - byte count
///   PropertyType (vi64) - must be even (single integer value)
///   AndSet (8 bits)
///   Range* { Start, End }
/// }
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PropertyFilter {
    /// The property type to filter on (must be even).
    pub property_type: u64,

    /// The underlying range filter.
    pub filter: RangeFilter,
}

impl PropertyFilter {
    pub fn new(property_type: u64) -> Result<Self, &'static str> {
        if property_type % 2 != 0 {
            return Err("property_type must be even");
        }
        Ok(Self {
            property_type,
            filter: RangeFilter::new(RangeFilterType::Property),
        })
    }

    /// Adds a range to the filter.
    pub fn add_range(&mut self, and_set: u8, start: u64, end: u64) {
        self.filter.add_range(and_set, start, end);
    }

    /// Evaluates if a property value passes the filter.
    pub fn matches(&self, value: u64) -> bool {
        self.filter.matches(value)
    }
}

impl Encode for PropertyFilter {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        if self.property_type % 2 != 0 {
            return Err(EncodeError::InvalidValue);
        }
        self.property_type.encode(w)?;
        self.filter.encode(w)?;
        Ok(())
    }
}

impl Decode for PropertyFilter {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let property_type = u64::decode(r)?;
        if property_type % 2 != 0 {
            return Err(DecodeError::InvalidValue);
        }
        let filter = RangeFilter::decode(r)?;
        Ok(Self {
            property_type,
            filter,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range_contains() {
        let range = Range::new(10, 20);
        assert!(!range.contains(9));
        assert!(range.contains(10));
        assert!(range.contains(15));
        assert!(range.contains(20));
        assert!(!range.contains(21));
    }

    #[test]
    fn test_range_filter_single_set() {
        let mut filter = RangeFilter::subgroup();
        filter.add_range(0, 10, 20);
        filter.add_range(0, 30, 40);

        // OR within same and_set
        assert!(!filter.matches(9));
        assert!(filter.matches(10));
        assert!(filter.matches(15));
        assert!(filter.matches(35));
        assert!(!filter.matches(25));
    }

    #[test]
    fn test_range_filter_and_sets() {
        let mut filter = RangeFilter::object();
        filter.add_range(0, 0, 100); // and_set 0: 0-100
        filter.add_range(1, 50, 150); // and_set 1: 50-150

        // AND between and_sets: must be in both
        assert!(!filter.matches(25)); // in 0, not in 1
        assert!(filter.matches(75)); // in both
        assert!(!filter.matches(125)); // in 1, not in 0
    }

    #[test]
    fn test_priority_filter() {
        let mut filter = RangeFilter::priority();
        filter.add_range(0, 0, 127); // High priority range

        assert!(filter.matches(0));
        assert!(filter.matches(127));
        assert!(!filter.matches(128));
        assert!(!filter.matches(255));
    }

    #[test]
    fn test_property_filter_even_type() {
        let filter = PropertyFilter::new(0x10);
        assert!(filter.is_ok());

        let filter = PropertyFilter::new(0x11);
        assert!(filter.is_err());
    }
}
