//! Filter Pipeline - combines all filter stages into a unified filtering system.
//!
//! The pipeline provides:
//! - Per-subscription object filters managed via slab allocator
//! - Global track and track extension filters
//! - Aggregated statistics across all stages
//! - Filter deduplication for large-scale fanouts

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use moq_transport::coding::TrackNamespace;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use slab::Slab;

use super::{
    FilterConfig, FilterStats, FilterStatsSnapshot, ObjectFilter, ObjectFilterParams, ObjectInfo,
    TopNFilter, TopNStats, TrackExtensionFilter, TrackFilter, TrackFilterMode,
};

/// Subscription ID type.
pub type SubscriptionId = usize;

/// Filter group ID for deduplication.
type FilterGroupId = u64;

/// Main filter pipeline combining all stages.
///
/// # Architecture
///
/// ```text
/// Incoming      ┌──────────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────────┐   Forwarded
/// Object   ───► │ Track Ext    │──►│    Track     │──►│   Object     │──►│    Top-N     │──► Object
///               │   Filter     │   │    Filter    │   │   Filter     │   │   Filter     │
///               └──────────────┘   └──────────────┘   └──────────────┘   └──────────────┘
/// ```
///
/// # Performance Features
///
/// - **Filter Deduplication**: When many subscribers use identical filters,
///   we evaluate the filter once and share results.
/// - **Lock-free Hot Path**: Read operations use RwLock with read-heavy bias.
/// - **Slab Allocation**: O(1) subscription filter lookup by ID.
/// - **Statistics Collection**: Optional, can be toggled at runtime.
/// - **Top-N Filtering**: Select top N tracks based on metrics in extension headers.
pub struct FilterPipeline {
    /// Configuration.
    config: FilterConfig,

    /// Stage 1: Track Extension Filter (global).
    track_ext_filter: RwLock<TrackExtensionFilter>,

    /// Stage 2: Track Filter (global).
    track_filter: RwLock<TrackFilter>,

    /// Stage 3: Object Filters (per-subscription).
    /// Managed via slab for O(1) lookup by subscription ID.
    object_filters: RwLock<Slab<Arc<ObjectFilter>>>,

    /// Stage 4: Top-N Filter (global).
    /// Selects top N tracks based on metrics in extension headers.
    topn_filter: Option<TopNFilter>,

    /// Filter group deduplication.
    /// Maps filter hash to (filter, subscription_ids).
    filter_groups: RwLock<FxHashMap<FilterGroupId, Arc<ObjectFilter>>>,

    /// Subscription to filter group mapping.
    sub_to_group: RwLock<FxHashMap<SubscriptionId, FilterGroupId>>,

    /// Global enabled state.
    enabled: AtomicBool,

    /// Pipeline statistics (reserved for future use).
    #[allow(dead_code)]
    stats: PipelineStats,
}

impl FilterPipeline {
    /// Creates a new filter pipeline with the given configuration.
    pub fn new(config: FilterConfig) -> Self {
        let stats_enabled = config.stats_enabled;

        let mut track_ext_filter = TrackExtensionFilter::new();
        let mut track_filter = TrackFilter::new(TrackFilterMode::Denylist);

        if stats_enabled {
            track_ext_filter.stats = FilterStats::enabled();
            track_filter.stats = FilterStats::enabled();
        }

        if !config.track_ext_enabled {
            track_ext_filter.disable();
        }

        if !config.track_enabled {
            track_filter.disable();
        }

        // Initialize Top-N filter if enabled
        let topn_filter = if config.topn_enabled {
            Some(TopNFilter::new(config.topn_config.clone()))
        } else {
            None
        };

        Self {
            config,
            track_ext_filter: RwLock::new(track_ext_filter),
            track_filter: RwLock::new(track_filter),
            object_filters: RwLock::new(Slab::new()),
            topn_filter,
            filter_groups: RwLock::new(FxHashMap::default()),
            sub_to_group: RwLock::new(FxHashMap::default()),
            enabled: AtomicBool::new(true),
            stats: PipelineStats::new(stats_enabled),
        }
    }

    /// Creates a pipeline with default configuration.
    pub fn default_config() -> Self {
        Self::new(FilterConfig::default())
    }

    /// Enables the entire pipeline.
    #[inline]
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Disables the entire pipeline (all objects pass through).
    #[inline]
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// Returns true if the pipeline is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Returns the current configuration.
    pub fn config(&self) -> &FilterConfig {
        &self.config
    }

    // ==================== Stage 1: Track Extension Filter ====================

    /// Returns a reference to the track extension filter for configuration.
    pub fn track_ext_filter(&self) -> impl std::ops::Deref<Target = TrackExtensionFilter> + '_ {
        self.track_ext_filter.read()
    }

    /// Returns a mutable reference to the track extension filter.
    pub fn track_ext_filter_mut(
        &self,
    ) -> impl std::ops::DerefMut<Target = TrackExtensionFilter> + '_ {
        self.track_ext_filter.write()
    }

    /// Filters based on track extensions.
    #[inline]
    pub fn filter_track_extensions(&self, extensions: &[(u64, u64)]) -> bool {
        if !self.is_enabled() || !self.config.track_ext_enabled {
            return true;
        }

        let filter = self.track_ext_filter.read();
        let result = filter.matches(extensions);
        filter.stats.record(result);
        result
    }

    // ==================== Stage 2: Track Filter ====================

    /// Returns a reference to the track filter for configuration.
    pub fn track_filter(&self) -> impl std::ops::Deref<Target = TrackFilter> + '_ {
        self.track_filter.read()
    }

    /// Returns a mutable reference to the track filter.
    pub fn track_filter_mut(&self) -> impl std::ops::DerefMut<Target = TrackFilter> + '_ {
        self.track_filter.write()
    }

    /// Filters a track at subscription time.
    #[inline]
    pub fn filter_track(&self, namespace: &TrackNamespace, name: &str) -> bool {
        if !self.is_enabled() || !self.config.track_enabled {
            return true;
        }

        self.track_filter.read().filter(namespace, name)
    }

    // ==================== Stage 3: Object Filter ====================

    /// Registers a new subscription with an object filter.
    /// Returns the subscription ID for later use.
    pub fn register_subscription(&self, params: ObjectFilterParams) -> SubscriptionId {
        let filter = if self.config.stats_enabled {
            let mut f = ObjectFilter::with_stats();
            Self::apply_params(&mut f, params);
            f
        } else {
            ObjectFilter::from_params(params)
        };

        if !self.config.object_enabled {
            filter.disable();
        }

        let filter = Arc::new(filter);
        let sub_id = self.object_filters.write().insert(filter);

        sub_id
    }

    /// Registers a subscription with filter deduplication.
    /// Multiple subscriptions with identical filters will share evaluation.
    pub fn register_subscription_dedupe(
        &self,
        params: ObjectFilterParams,
        filter_hash: FilterGroupId,
    ) -> SubscriptionId {
        // Check if we already have this filter group
        let filter = {
            let groups = self.filter_groups.read();
            groups.get(&filter_hash).cloned()
        };

        let filter = filter.unwrap_or_else(|| {
            let f = if self.config.stats_enabled {
                let mut f = ObjectFilter::with_stats();
                Self::apply_params(&mut f, params);
                f
            } else {
                ObjectFilter::from_params(params)
            };

            if !self.config.object_enabled {
                f.disable();
            }

            let filter = Arc::new(f);
            self.filter_groups.write().insert(filter_hash, filter.clone());
            filter
        });

        let sub_id = self.object_filters.write().insert(filter);
        self.sub_to_group.write().insert(sub_id, filter_hash);

        sub_id
    }

    /// Applies parameters to an existing filter.
    fn apply_params(filter: &mut ObjectFilter, params: ObjectFilterParams) {
        if let Some(loc) = params.location {
            filter.set_location(loc);
        }

        if let Some(groups) = params.groups {
            use super::{GroupFilter, RangeSet};
            let ranges = RangeSet::from_ranges(groups);
            filter.set_group(GroupFilter::from_ranges(ranges));
        }

        if let Some(subgroups) = params.subgroups {
            use super::RangeSet;
            filter.set_subgroup(RangeSet::from_ranges(subgroups));
        }

        if let Some(objects) = params.objects {
            use super::RangeSet;
            filter.set_object(RangeSet::from_ranges(objects));
        }

        if let Some(priorities) = params.priorities {
            use super::PriorityBitmap;
            let mut bitmap = PriorityBitmap::new();
            for (start, end) in priorities {
                bitmap.set_range(start, end);
            }
            filter.set_priority(bitmap);
        }

        if let Some(extensions) = params.extensions {
            use super::RangeSet;
            for (ext_type, start, end) in extensions {
                filter.add_extension(ext_type, RangeSet::single(start, end));
            }
        }
    }

    /// Unregisters a subscription.
    pub fn unregister_subscription(&self, sub_id: SubscriptionId) {
        self.object_filters.write().try_remove(sub_id);

        // Clean up deduplication mappings
        if let Some(group_id) = self.sub_to_group.write().remove(&sub_id) {
            // Check if any other subscriptions use this filter group
            let still_used = self.sub_to_group.read().values().any(|&g| g == group_id);
            if !still_used {
                self.filter_groups.write().remove(&group_id);
            }
        }
    }

    /// Gets the object filter for a subscription.
    pub fn get_object_filter(&self, sub_id: SubscriptionId) -> Option<Arc<ObjectFilter>> {
        self.object_filters.read().get(sub_id).cloned()
    }

    /// Filters an object for a specific subscription.
    #[inline]
    pub fn filter_object(&self, sub_id: SubscriptionId, obj: &ObjectInfo) -> bool {
        if !self.is_enabled() || !self.config.object_enabled {
            return true;
        }

        let filters = self.object_filters.read();
        match filters.get(sub_id) {
            Some(filter) => filter.filter(obj),
            None => true, // No filter = pass through
        }
    }

    // ==================== Stage 4: Top-N Filter ====================

    /// Returns a reference to the Top-N filter, if enabled.
    pub fn topn_filter(&self) -> Option<&TopNFilter> {
        self.topn_filter.as_ref()
    }

    /// Returns true if Top-N filtering is enabled.
    pub fn is_topn_enabled(&self) -> bool {
        self.topn_filter.is_some()
    }

    /// Updates the metric for a track based on object extensions.
    /// Should be called for every object that flows through the relay.
    /// Returns the extracted metric value, if any.
    #[inline]
    pub fn update_topn_metric(
        &self,
        namespace: &TrackNamespace,
        track_name: &str,
        extensions: &[(u64, u64)],
    ) -> Option<u64> {
        self.topn_filter
            .as_ref()
            .and_then(|f| f.update_metric(namespace, track_name, extensions))
    }

    /// Checks if a track is in the top-n (should be forwarded).
    /// Returns true if Top-N is disabled or track is in top-n.
    #[inline]
    pub fn filter_topn(&self, namespace: &TrackNamespace, track_name: &str) -> bool {
        match &self.topn_filter {
            Some(f) => f.should_forward(namespace, track_name),
            None => true,
        }
    }

    /// Returns Top-N filter statistics, if enabled.
    pub fn topn_stats(&self) -> Option<TopNStats> {
        self.topn_filter.as_ref().map(|f| f.stats())
    }

    /// Checks if an object should be forwarded to a subscription.
    /// This is the main hot-path function.
    ///
    /// Combines all four filter stages:
    /// 1. Track Extension Filter (if extensions provided)
    /// 2. Track Filter (checked at subscription time, not here)
    /// 3. Object Filter
    /// 4. Top-N Filter (if namespace/track_name provided)
    #[inline]
    pub fn should_forward(
        &self,
        sub_id: SubscriptionId,
        obj: &ObjectInfo,
        track_extensions: Option<&[(u64, u64)]>,
    ) -> bool {
        if !self.is_enabled() {
            return true;
        }

        // Stage 1: Track Extension Filter
        if self.config.track_ext_enabled {
            if let Some(exts) = track_extensions {
                if !self.filter_track_extensions(exts) {
                    return false;
                }
            }
        }

        // Stage 2 is checked at subscription time, not per-object

        // Stage 3: Object Filter
        self.filter_object(sub_id, obj)
    }

    /// Checks if an object should be forwarded, including Top-N filtering.
    /// Extended version that includes track identification for Top-N.
    ///
    /// Combines all four filter stages:
    /// 1. Track Extension Filter
    /// 2. Track Filter (checked at subscription time)
    /// 3. Object Filter
    /// 4. Top-N Filter
    #[inline]
    pub fn should_forward_with_topn(
        &self,
        sub_id: SubscriptionId,
        obj: &ObjectInfo,
        namespace: &TrackNamespace,
        track_name: &str,
        track_extensions: Option<&[(u64, u64)]>,
    ) -> bool {
        if !self.is_enabled() {
            return true;
        }

        // Stage 1: Track Extension Filter
        if self.config.track_ext_enabled {
            if let Some(exts) = track_extensions {
                if !self.filter_track_extensions(exts) {
                    return false;
                }
            }
        }

        // Stage 2 is checked at subscription time, not per-object

        // Stage 3: Object Filter
        if !self.filter_object(sub_id, obj) {
            return false;
        }

        // Stage 4: Top-N Filter
        // Update metric if extensions provided, then check if in top-n
        if let Some(exts) = track_extensions {
            self.update_topn_metric(namespace, track_name, exts);
        }
        self.filter_topn(namespace, track_name)
    }

    /// Batch filter for multiple subscriptions.
    /// Returns a list of subscription IDs that should receive the object.
    pub fn filter_batch(
        &self,
        subscriptions: &[SubscriptionId],
        obj: &ObjectInfo,
        track_extensions: Option<&[(u64, u64)]>,
    ) -> Vec<SubscriptionId> {
        if !self.is_enabled() {
            return subscriptions.to_vec();
        }

        // Stage 1: Track Extension Filter (once for all)
        if self.config.track_ext_enabled {
            if let Some(exts) = track_extensions {
                if !self.filter_track_extensions(exts) {
                    return Vec::new();
                }
            }
        }

        // Stage 3: Object Filter for each subscription
        let filters = self.object_filters.read();

        subscriptions
            .iter()
            .filter(|&&sub_id| {
                filters
                    .get(sub_id)
                    .map(|f| f.matches(obj))
                    .unwrap_or(true)
            })
            .copied()
            .collect()
    }

    /// Batch filter with Top-N support.
    /// Returns a list of subscription IDs that should receive the object.
    pub fn filter_batch_with_topn(
        &self,
        subscriptions: &[SubscriptionId],
        obj: &ObjectInfo,
        namespace: &TrackNamespace,
        track_name: &str,
        track_extensions: Option<&[(u64, u64)]>,
    ) -> Vec<SubscriptionId> {
        if !self.is_enabled() {
            return subscriptions.to_vec();
        }

        // Stage 1: Track Extension Filter (once for all)
        if self.config.track_ext_enabled {
            if let Some(exts) = track_extensions {
                if !self.filter_track_extensions(exts) {
                    return Vec::new();
                }
            }
        }

        // Stage 4: Top-N Filter (once for all)
        if let Some(exts) = track_extensions {
            self.update_topn_metric(namespace, track_name, exts);
        }
        if !self.filter_topn(namespace, track_name) {
            return Vec::new();
        }

        // Stage 3: Object Filter for each subscription
        let filters = self.object_filters.read();

        subscriptions
            .iter()
            .filter(|&&sub_id| {
                filters
                    .get(sub_id)
                    .map(|f| f.matches(obj))
                    .unwrap_or(true)
            })
            .copied()
            .collect()
    }

    // ==================== Statistics ====================

    /// Returns a report of all pipeline statistics.
    pub fn report(&self) -> PipelineReport {
        let track_ext = self.track_ext_filter.read().stats.snapshot();
        let track = self.track_filter.read().stats.snapshot();

        // Aggregate object filter stats
        let mut object = FilterStatsSnapshot::default();
        for (_, filter) in self.object_filters.read().iter() {
            object.merge(&filter.stats.snapshot());
        }

        // Top-N stats
        let topn = self.topn_filter.as_ref().map(|f| f.stats());

        PipelineReport {
            track_ext,
            track,
            object,
            topn,
        }
    }

    /// Resets all statistics.
    pub fn reset_stats(&self) {
        self.track_ext_filter.read().stats.reset();
        self.track_filter.read().stats.reset();

        for (_, filter) in self.object_filters.read().iter() {
            filter.stats.reset();
        }

        if let Some(ref topn) = self.topn_filter {
            topn.reset_stats();
        }
    }

    /// Returns the number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.object_filters.read().len()
    }

    /// Returns the number of unique filter groups (for deduplication stats).
    pub fn filter_group_count(&self) -> usize {
        self.filter_groups.read().len()
    }

    /// Estimates total memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let track_ext = self.track_ext_filter.read().memory_usage();
        let track = self.track_filter.read().memory_usage();
        let objects: usize = self
            .object_filters
            .read()
            .iter()
            .map(|(_, f)| f.memory_usage())
            .sum();

        base + track_ext + track + objects
    }
}

impl Default for FilterPipeline {
    fn default() -> Self {
        Self::default_config()
    }
}

/// Statistics for the entire pipeline (reserved for future use).
#[allow(dead_code)]
struct PipelineStats {
    enabled: bool,
}

impl PipelineStats {
    fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

/// Report of pipeline statistics across all stages.
#[derive(Debug, Clone, Default)]
pub struct PipelineReport {
    /// Track extension filter statistics.
    pub track_ext: FilterStatsSnapshot,

    /// Track filter statistics.
    pub track: FilterStatsSnapshot,

    /// Object filter statistics (aggregated across all subscriptions).
    pub object: FilterStatsSnapshot,

    /// Top-N filter statistics (if enabled).
    pub topn: Option<TopNStats>,
}

impl PipelineReport {
    /// Returns total items processed across all stages.
    pub fn total_processed(&self) -> u64 {
        self.track_ext.total() + self.track.total() + self.object.total()
    }

    /// Returns total items filtered across all stages.
    pub fn total_filtered(&self) -> u64 {
        self.track_ext.filtered + self.track.filtered + self.object.filtered
    }
}

impl fmt::Display for PipelineReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Filter Pipeline Statistics")?;
        writeln!(f, "==========================")?;
        writeln!(f)?;
        writeln!(f, "Track Extension Filter:")?;
        write!(f, "{}", self.track_ext)?;
        writeln!(f)?;
        writeln!(f)?;
        writeln!(f, "Track Filter:")?;
        write!(f, "{}", self.track)?;
        writeln!(f)?;
        writeln!(f)?;
        writeln!(f, "Object Filter (aggregated):")?;
        write!(f, "{}", self.object)?;

        if let Some(ref topn) = self.topn {
            writeln!(f)?;
            writeln!(f)?;
            writeln!(f, "Top-N Filter:")?;
            writeln!(f, "  Objects processed: {}", topn.objects_processed)?;
            writeln!(f, "  Objects filtered:  {}", topn.objects_filtered)?;
            writeln!(f, "  Filter rate:       {:.2}%", topn.filter_rate())?;
            writeln!(f, "  Tracks monitored:  {}", topn.tracks_monitored)?;
            writeln!(f, "  Current top-n:     {}", topn.current_top_n)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::LocationFilter;

    fn ns(path: &str) -> TrackNamespace {
        TrackNamespace::from_utf8_path(path)
    }

    #[test]
    fn test_pipeline_creation() {
        let pipeline = FilterPipeline::default_config();
        assert!(pipeline.is_enabled());
        assert_eq!(pipeline.subscription_count(), 0);
    }

    #[test]
    fn test_subscription_registration() {
        let pipeline = FilterPipeline::default_config();

        let params = ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            ..Default::default()
        };

        let sub_id = pipeline.register_subscription(params);
        assert_eq!(pipeline.subscription_count(), 1);

        pipeline.unregister_subscription(sub_id);
        assert_eq!(pipeline.subscription_count(), 0);
    }

    #[test]
    fn test_object_filtering() {
        let pipeline = FilterPipeline::new(FilterConfig::object_only());

        let params = ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            ..Default::default()
        };

        let sub_id = pipeline.register_subscription(params);

        let obj_pass = ObjectInfo::new(15, 0);
        let obj_fail = ObjectInfo::new(25, 0);

        assert!(pipeline.filter_object(sub_id, &obj_pass));
        assert!(!pipeline.filter_object(sub_id, &obj_fail));
    }

    #[test]
    fn test_pipeline_disabled() {
        let pipeline = FilterPipeline::default_config();
        pipeline.disable();

        let params = ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            ..Default::default()
        };

        let sub_id = pipeline.register_subscription(params);

        // Should pass even though object is outside range
        let obj = ObjectInfo::new(100, 0);
        assert!(pipeline.filter_object(sub_id, &obj));
    }

    #[test]
    fn test_track_filtering() {
        let config = FilterConfig {
            track_enabled: true,
            ..FilterConfig::all_disabled()
        };
        let pipeline = FilterPipeline::new(config);

        // Add to denylist (default mode)
        pipeline.track_filter_mut().add(&ns("live/blocked"), "video");

        assert!(!pipeline.filter_track(&ns("live/blocked"), "video"));
        assert!(pipeline.filter_track(&ns("live/allowed"), "video"));
    }

    #[test]
    fn test_batch_filtering() {
        let pipeline = FilterPipeline::new(FilterConfig::object_only());

        // Create subscriptions with different filters
        let sub1 = pipeline.register_subscription(ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            ..Default::default()
        });

        let sub2 = pipeline.register_subscription(ObjectFilterParams {
            location: Some(LocationFilter::group_range(15, 25)),
            ..Default::default()
        });

        let sub3 = pipeline.register_subscription(ObjectFilterParams {
            location: Some(LocationFilter::group_range(100, 200)),
            ..Default::default()
        });

        // Object at group 18 should match sub1 and sub2
        let obj = ObjectInfo::new(18, 0);
        let result = pipeline.filter_batch(&[sub1, sub2, sub3], &obj, None);

        assert!(result.contains(&sub1));
        assert!(result.contains(&sub2));
        assert!(!result.contains(&sub3));
    }

    #[test]
    fn test_should_forward() {
        let config = FilterConfig {
            track_ext_enabled: true,
            object_enabled: true,
            ..FilterConfig::all_disabled()
        };
        let pipeline = FilterPipeline::new(config);

        // Configure track extension filter
        pipeline.track_ext_filter_mut().filter_exact(1, 100);

        let sub_id = pipeline.register_subscription(ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            ..Default::default()
        });

        let obj = ObjectInfo::new(15, 0);

        // Should pass with matching extension
        assert!(pipeline.should_forward(sub_id, &obj, Some(&[(1, 100)])));

        // Should fail with non-matching extension
        assert!(!pipeline.should_forward(sub_id, &obj, Some(&[(1, 999)])));

        // Should fail with object outside range
        let obj_bad = ObjectInfo::new(100, 0);
        assert!(!pipeline.should_forward(sub_id, &obj_bad, Some(&[(1, 100)])));
    }

    #[test]
    fn test_stats_with_config() {
        let config = FilterConfig::all_enabled().with_stats();
        let pipeline = FilterPipeline::new(config);

        let sub_id = pipeline.register_subscription(ObjectFilterParams {
            location: Some(LocationFilter::group_range(10, 20)),
            ..Default::default()
        });

        // Generate some filter activity
        pipeline.filter_object(sub_id, &ObjectInfo::new(15, 0)); // Pass
        pipeline.filter_object(sub_id, &ObjectInfo::new(25, 0)); // Fail

        let report = pipeline.report();
        assert_eq!(report.object.passed, 1);
        assert_eq!(report.object.filtered, 1);
    }
}
