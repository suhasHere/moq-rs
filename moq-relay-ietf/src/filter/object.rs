//! Object Filter implementation.
//!
//! High-performance object filtering for the forwarding hot path.
//! Optimized for cache efficiency and minimal branching.

use std::sync::atomic::{AtomicBool, Ordering};

use super::{FilterStats, RangeSet};

/// Priority bitmap for O(1) priority filtering.
///
/// Since priority is u8 (0-255), we use a fixed-size 256-bit bitmap
/// stored as 4 x u64.
///
/// # Memory
/// - 32 bytes fixed size
/// - No heap allocation
#[derive(Clone, Debug, Default)]
pub struct PriorityBitmap {
    bits: [u64; 4],
}

impl PriorityBitmap {
    /// Creates an empty bitmap (no priorities allowed).
    #[inline]
    pub const fn new() -> Self {
        Self { bits: [0; 4] }
    }

    /// Creates a bitmap that allows all priorities.
    #[inline]
    pub const fn all() -> Self {
        Self {
            bits: [u64::MAX; 4],
        }
    }

    /// Creates a bitmap from a range of priorities.
    pub fn from_range(start: u8, end: u8) -> Self {
        let mut bitmap = Self::new();
        bitmap.set_range(start, end);
        bitmap
    }

    /// Sets a single priority as allowed.
    #[inline]
    pub fn set(&mut self, priority: u8) {
        let idx = (priority / 64) as usize;
        let bit = priority % 64;
        self.bits[idx] |= 1 << bit;
    }

    /// Sets a range of priorities as allowed.
    pub fn set_range(&mut self, start: u8, end: u8) {
        for p in start..=end {
            self.set(p);
        }
    }

    /// Clears a single priority.
    #[inline]
    pub fn clear(&mut self, priority: u8) {
        let idx = (priority / 64) as usize;
        let bit = priority % 64;
        self.bits[idx] &= !(1 << bit);
    }

    /// Checks if a priority is allowed.
    #[inline(always)]
    pub fn contains(&self, priority: u8) -> bool {
        let idx = (priority / 64) as usize;
        let bit = priority % 64;
        (self.bits[idx] >> bit) & 1 == 1
    }

    /// Returns true if no priorities are allowed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bits == [0; 4]
    }

    /// Returns the count of allowed priorities.
    pub fn count(&self) -> u32 {
        self.bits.iter().map(|w| w.count_ones()).sum()
    }
}

/// Group filter with adaptive storage.
///
/// Uses sparse (RangeSet) for typical cases and can be extended
/// to use dense (roaring bitmap) for highly dense ranges.
#[derive(Clone, Debug)]
pub enum GroupFilter {
    /// For sparse ranges, use sorted range set.
    /// O(log r) lookup where r = number of ranges.
    Sparse(RangeSet),

    /// For dense ranges (>50% coverage), use direct range check.
    /// This is a simpler alternative to roaring bitmap.
    Dense {
        min: u64,
        max: u64,
        // For truly dense ranges, just store min/max
        // Any group_id in [min, max] is allowed
    },
}

impl GroupFilter {
    /// Creates a sparse group filter.
    pub fn sparse(ranges: RangeSet) -> Self {
        GroupFilter::Sparse(ranges)
    }

    /// Creates a dense group filter covering a contiguous range.
    pub fn dense(min: u64, max: u64) -> Self {
        GroupFilter::Dense { min, max }
    }

    /// Creates a group filter from ranges, choosing the best representation.
    pub fn from_ranges(ranges: RangeSet) -> Self {
        // If we have a single contiguous range, use dense
        if ranges.len() == 1 {
            if let Some(&(min, max)) = ranges.iter().next() {
                return GroupFilter::Dense { min, max };
            }
        }

        // Check if ranges are dense enough for simple min/max
        if ranges.is_dense() {
            if let Some(span) = ranges.span() {
                let min = ranges.iter().next().map(|(s, _)| *s).unwrap_or(0);
                let max = ranges.iter().last().map(|(_, e)| *e).unwrap_or(0);
                // Only use dense if coverage is very high
                if ranges.coverage() * 100 / (span + 1) >= 90 {
                    return GroupFilter::Dense { min, max };
                }
            }
        }

        GroupFilter::Sparse(ranges)
    }

    /// Checks if a group ID is allowed.
    #[inline]
    pub fn contains(&self, group_id: u64) -> bool {
        match self {
            GroupFilter::Sparse(ranges) => ranges.contains(group_id),
            GroupFilter::Dense { min, max } => group_id >= *min && group_id <= *max,
        }
    }
}

impl Default for GroupFilter {
    fn default() -> Self {
        GroupFilter::Sparse(RangeSet::new())
    }
}

/// Location filter for start/end position filtering.
///
/// # Memory
/// - 32 bytes fixed size (inline)
#[derive(Clone, Copy, Debug, Default)]
pub struct LocationFilter {
    pub start_group: u64,
    pub start_object: Option<u64>,
    pub end_group: Option<u64>,
    pub end_object: Option<u64>,
}

impl LocationFilter {
    /// Creates a new location filter.
    pub fn new(
        start_group: u64,
        start_object: Option<u64>,
        end_group: Option<u64>,
        end_object: Option<u64>,
    ) -> Self {
        Self {
            start_group,
            start_object,
            end_group,
            end_object,
        }
    }

    /// Creates a filter starting from a specific group.
    pub fn from_group(group: u64) -> Self {
        Self {
            start_group: group,
            start_object: None,
            end_group: None,
            end_object: None,
        }
    }

    /// Creates a filter for a range of groups.
    pub fn group_range(start: u64, end: u64) -> Self {
        Self {
            start_group: start,
            start_object: None,
            end_group: Some(end),
            end_object: None,
        }
    }

    /// Checks if a (group_id, object_id) pair matches this filter.
    ///
    /// Uses branchless comparison where possible for better CPU pipelining.
    #[inline]
    pub fn matches(&self, group_id: u64, object_id: u64) -> bool {
        // Check if after start
        let after_start = group_id > self.start_group
            || (group_id == self.start_group
                && self.start_object.map_or(true, |s| object_id >= s));

        if !after_start {
            return false;
        }

        // Check if before end
        match self.end_group {
            None => true,
            Some(eg) => {
                group_id < eg
                    || (group_id == eg && self.end_object.map_or(true, |eo| object_id <= eo))
            }
        }
    }
}

/// Parameters for creating an object filter.
#[derive(Clone, Debug, Default)]
pub struct ObjectFilterParams {
    pub location: Option<LocationFilter>,
    pub groups: Option<Vec<(u64, u64)>>,
    pub subgroups: Option<Vec<(u64, u64)>>,
    pub objects: Option<Vec<(u64, u64)>>,
    pub priorities: Option<Vec<(u8, u8)>>,
    pub extensions: Option<Vec<(u64, u64, u64)>>, // (ext_type, start, end)
}

/// Object header data needed for filtering.
#[derive(Clone, Debug)]
pub struct ObjectInfo {
    pub group_id: u64,
    pub subgroup_id: Option<u64>,
    pub object_id: u64,
    pub publisher_priority: u8,
    pub extensions: Option<Vec<(u64, u64)>>, // (type, value)
}

impl ObjectInfo {
    /// Creates a minimal object info for testing.
    pub fn new(group_id: u64, object_id: u64) -> Self {
        Self {
            group_id,
            subgroup_id: None,
            object_id,
            publisher_priority: 0,
            extensions: None,
        }
    }

    /// Sets the subgroup ID.
    pub fn with_subgroup(mut self, subgroup_id: u64) -> Self {
        self.subgroup_id = Some(subgroup_id);
        self
    }

    /// Sets the priority.
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.publisher_priority = priority;
        self
    }
}

/// High-performance object filter optimized for the forwarding hot path.
///
/// Filters individual objects based on location, group, subgroup, object ID,
/// priority, and extension values.
///
/// # Memory Layout
/// Optimized for cache efficiency with hot fields first:
/// - enabled flag (1 byte + padding)
/// - location filter (32 bytes, inline, checked first)
/// - priority bitmap (32 bytes, inline, O(1) check)
/// - other filters (heap pointers)
///
/// # Performance
/// - Location filter: O(1) with branchless comparison
/// - Priority filter: O(1) bitmap lookup
/// - Group filter: O(1) dense or O(log r) sparse
/// - Subgroup/Object filter: O(log r)
pub struct ObjectFilter {
    /// Whether this filter is enabled.
    enabled: AtomicBool,

    /// Location filter (most common, checked first).
    location: Option<LocationFilter>,

    /// Priority filter using bitmap.
    priority: Option<PriorityBitmap>,

    /// Group filter.
    group: Option<GroupFilter>,

    /// Subgroup filter.
    subgroup: Option<RangeSet>,

    /// Object ID filter.
    object: Option<RangeSet>,

    /// Extension filters indexed by extension type.
    /// Using Vec instead of HashMap for small counts (typically 0-3 extensions).
    extensions: Option<Vec<(u64, RangeSet)>>,

    /// Statistics collector.
    pub stats: FilterStats,
}

impl ObjectFilter {
    /// Creates a new object filter with no filters set.
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            location: None,
            priority: None,
            group: None,
            subgroup: None,
            object: None,
            extensions: None,
            stats: FilterStats::new(),
        }
    }

    /// Creates an object filter with statistics enabled.
    pub fn with_stats() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            location: None,
            priority: None,
            group: None,
            subgroup: None,
            object: None,
            extensions: None,
            stats: FilterStats::enabled(),
        }
    }

    /// Creates an object filter from parameters.
    pub fn from_params(params: ObjectFilterParams) -> Self {
        let mut filter = Self::new();

        filter.location = params.location;

        if let Some(groups) = params.groups {
            let ranges = RangeSet::from_ranges(groups);
            filter.group = Some(GroupFilter::from_ranges(ranges));
        }

        if let Some(subgroups) = params.subgroups {
            filter.subgroup = Some(RangeSet::from_ranges(subgroups));
        }

        if let Some(objects) = params.objects {
            filter.object = Some(RangeSet::from_ranges(objects));
        }

        if let Some(priorities) = params.priorities {
            let mut bitmap = PriorityBitmap::new();
            for (start, end) in priorities {
                bitmap.set_range(start, end);
            }
            filter.priority = Some(bitmap);
        }

        if let Some(extensions) = params.extensions {
            let mut ext_filters = Vec::new();
            for (ext_type, start, end) in extensions {
                ext_filters.push((ext_type, RangeSet::single(start, end)));
            }
            if !ext_filters.is_empty() {
                filter.extensions = Some(ext_filters);
            }
        }

        filter
    }

    /// Enables this filter.
    #[inline]
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Disables this filter.
    #[inline]
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// Returns true if this filter is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Sets the location filter.
    pub fn set_location(&mut self, location: LocationFilter) {
        self.location = Some(location);
    }

    /// Sets the priority filter.
    pub fn set_priority(&mut self, bitmap: PriorityBitmap) {
        self.priority = Some(bitmap);
    }

    /// Sets the group filter.
    pub fn set_group(&mut self, filter: GroupFilter) {
        self.group = Some(filter);
    }

    /// Sets the subgroup filter.
    pub fn set_subgroup(&mut self, ranges: RangeSet) {
        self.subgroup = Some(ranges);
    }

    /// Sets the object filter.
    pub fn set_object(&mut self, ranges: RangeSet) {
        self.object = Some(ranges);
    }

    /// Adds an extension filter.
    pub fn add_extension(&mut self, ext_type: u64, ranges: RangeSet) {
        match &mut self.extensions {
            Some(exts) => {
                // Check if we already have this extension type
                for (t, r) in exts.iter_mut() {
                    if *t == ext_type {
                        // Merge ranges
                        for (start, end) in ranges.iter() {
                            r.insert(*start, *end);
                        }
                        return;
                    }
                }
                exts.push((ext_type, ranges));
            }
            None => {
                self.extensions = Some(vec![(ext_type, ranges)]);
            }
        }
    }

    /// Main matching function - inlined for hot path performance.
    ///
    /// Checks filters in order of typical selectivity:
    /// 1. Location (most common filter)
    /// 2. Priority (O(1) bitmap check)
    /// 3. Group
    /// 4. Subgroup
    /// 5. Object
    /// 6. Extensions (slowest, checked last)
    #[inline]
    pub fn matches(&self, obj: &ObjectInfo) -> bool {
        if !self.is_enabled() {
            return true;
        }

        // Location filter (most common, check first)
        if let Some(ref loc) = self.location {
            if !loc.matches(obj.group_id, obj.object_id) {
                return false;
            }
        }

        // Priority filter (O(1) bitmap lookup)
        if let Some(ref prio) = self.priority {
            if !prio.contains(obj.publisher_priority) {
                return false;
            }
        }

        // Group filter
        if let Some(ref group) = self.group {
            if !group.contains(obj.group_id) {
                return false;
            }
        }

        // Subgroup filter
        if let Some(ref subgroup) = self.subgroup {
            if let Some(sg_id) = obj.subgroup_id {
                if !subgroup.contains(sg_id) {
                    return false;
                }
            }
        }

        // Object filter
        if let Some(ref object) = self.object {
            if !object.contains(obj.object_id) {
                return false;
            }
        }

        // Extension filters (slowest, check last)
        if let Some(ref ext_filters) = self.extensions {
            if let Some(ref obj_exts) = obj.extensions {
                for (ext_type, range) in ext_filters {
                    for (t, v) in obj_exts {
                        if t == ext_type && !range.contains(*v) {
                            return false;
                        }
                    }
                }
            }
        }

        true
    }

    /// Filters an object and records statistics.
    #[inline]
    pub fn filter(&self, obj: &ObjectInfo) -> bool {
        let result = self.matches(obj);
        self.stats.record(result);
        result
    }

    /// Estimates memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();

        if let Some(ref group) = self.group {
            size += match group {
                GroupFilter::Sparse(ranges) => ranges.len() * 16,
                GroupFilter::Dense { .. } => 16,
            };
        }

        if let Some(ref subgroup) = self.subgroup {
            size += subgroup.len() * 16;
        }

        if let Some(ref object) = self.object {
            size += object.len() * 16;
        }

        if let Some(ref extensions) = self.extensions {
            for (_, ranges) in extensions {
                size += 8 + ranges.len() * 16;
            }
        }

        size
    }

    /// Returns true if any filters are configured.
    pub fn has_filters(&self) -> bool {
        self.location.is_some()
            || self.priority.is_some()
            || self.group.is_some()
            || self.subgroup.is_some()
            || self.object.is_some()
            || self.extensions.is_some()
    }
}

impl Default for ObjectFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_bitmap() {
        let mut bitmap = PriorityBitmap::new();
        bitmap.set_range(10, 20);

        assert!(!bitmap.contains(9));
        assert!(bitmap.contains(10));
        assert!(bitmap.contains(15));
        assert!(bitmap.contains(20));
        assert!(!bitmap.contains(21));
    }

    #[test]
    fn test_priority_bitmap_all() {
        let bitmap = PriorityBitmap::all();
        for p in 0..=255 {
            assert!(bitmap.contains(p));
        }
    }

    #[test]
    fn test_location_filter() {
        let filter = LocationFilter::group_range(10, 20);

        assert!(!filter.matches(9, 0));
        assert!(filter.matches(10, 0));
        assert!(filter.matches(15, 50));
        assert!(filter.matches(20, 100));
        assert!(!filter.matches(21, 0));
    }

    #[test]
    fn test_location_filter_with_objects() {
        let filter = LocationFilter::new(10, Some(5), Some(20), Some(10));

        assert!(!filter.matches(10, 4)); // Before start object
        assert!(filter.matches(10, 5)); // At start
        assert!(filter.matches(15, 0)); // Middle group
        assert!(filter.matches(20, 10)); // At end
        assert!(!filter.matches(20, 11)); // After end object
    }

    #[test]
    fn test_object_filter_empty() {
        let filter = ObjectFilter::new();
        let obj = ObjectInfo::new(100, 50);

        // Empty filter should pass everything
        assert!(filter.matches(&obj));
    }

    #[test]
    fn test_object_filter_location() {
        let mut filter = ObjectFilter::new();
        filter.set_location(LocationFilter::group_range(10, 20));

        assert!(!filter.matches(&ObjectInfo::new(9, 0)));
        assert!(filter.matches(&ObjectInfo::new(15, 50)));
        assert!(!filter.matches(&ObjectInfo::new(21, 0)));
    }

    #[test]
    fn test_object_filter_priority() {
        let mut filter = ObjectFilter::new();
        filter.set_priority(PriorityBitmap::from_range(0, 127));

        assert!(filter.matches(&ObjectInfo::new(0, 0).with_priority(0)));
        assert!(filter.matches(&ObjectInfo::new(0, 0).with_priority(127)));
        assert!(!filter.matches(&ObjectInfo::new(0, 0).with_priority(128)));
        assert!(!filter.matches(&ObjectInfo::new(0, 0).with_priority(255)));
    }

    #[test]
    fn test_object_filter_combined() {
        let mut filter = ObjectFilter::new();
        filter.set_location(LocationFilter::group_range(10, 20));
        filter.set_priority(PriorityBitmap::from_range(0, 100));

        // Must pass both filters
        assert!(filter.matches(&ObjectInfo::new(15, 0).with_priority(50)));
        assert!(!filter.matches(&ObjectInfo::new(15, 0).with_priority(150))); // Bad priority
        assert!(!filter.matches(&ObjectInfo::new(25, 0).with_priority(50))); // Bad group
    }

    #[test]
    fn test_object_filter_disabled() {
        let mut filter = ObjectFilter::new();
        filter.set_location(LocationFilter::group_range(10, 20));
        filter.disable();

        // Should pass everything when disabled
        assert!(filter.matches(&ObjectInfo::new(100, 0)));
    }

    #[test]
    fn test_object_filter_stats() {
        let mut filter = ObjectFilter::with_stats();
        filter.set_location(LocationFilter::group_range(10, 20));

        filter.filter(&ObjectInfo::new(15, 0)); // Pass
        filter.filter(&ObjectInfo::new(15, 0)); // Pass
        filter.filter(&ObjectInfo::new(25, 0)); // Fail

        let stats = filter.stats.snapshot();
        assert_eq!(stats.passed, 2);
        assert_eq!(stats.filtered, 1);
    }

    #[test]
    fn test_group_filter_sparse() {
        let ranges = RangeSet::from_ranges([(1, 5), (10, 15), (100, 200)]);
        let filter = GroupFilter::sparse(ranges);

        assert!(filter.contains(3));
        assert!(filter.contains(12));
        assert!(filter.contains(150));
        assert!(!filter.contains(50));
    }

    #[test]
    fn test_group_filter_dense() {
        let filter = GroupFilter::dense(10, 100);

        assert!(!filter.contains(9));
        assert!(filter.contains(10));
        assert!(filter.contains(50));
        assert!(filter.contains(100));
        assert!(!filter.contains(101));
    }

    #[test]
    fn test_from_params() {
        let params = ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            priorities: Some(vec![(0, 127)]),
            groups: None,
            subgroups: None,
            objects: None,
            extensions: None,
        };

        let filter = ObjectFilter::from_params(params);

        assert!(filter.matches(&ObjectInfo::new(15, 0).with_priority(50)));
        assert!(!filter.matches(&ObjectInfo::new(25, 0).with_priority(50)));
    }
}
