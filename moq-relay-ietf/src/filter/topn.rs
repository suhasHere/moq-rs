//! Top-N Filter for metric-based track selection.
//!
//! Selects the top N tracks based on a metric value carried in object extension headers.
//! Useful for scenarios like:
//! - Active speaker detection (top N loudest audio tracks)
//! - Activity-based filtering (top N most active video tracks)
//!
//! # Algorithm
//! - Each track's metric is updated when objects arrive
//! - Metrics decay over time to handle tracks that stop sending
//! - Top-N selection is computed on demand with caching

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use moq_transport::coding::TrackNamespace;
use parking_lot::RwLock;

/// Default extension type for metric values (can be configured)
pub const DEFAULT_METRIC_EXTENSION_TYPE: u64 = 0x100;

/// Configuration for the Top-N filter.
#[derive(Clone, Debug)]
pub struct TopNConfig {
    /// Number of top tracks to select
    pub n: usize,

    /// Extension type that carries the metric value
    pub metric_extension_type: u64,

    /// How long before a track's metric starts decaying (no updates)
    pub decay_after: Duration,

    /// How often to recompute the top-n selection
    pub recompute_interval: Duration,

    /// Whether higher metric values are better (true) or lower (false)
    pub higher_is_better: bool,
}

impl Default for TopNConfig {
    fn default() -> Self {
        Self {
            n: 3,
            metric_extension_type: DEFAULT_METRIC_EXTENSION_TYPE,
            decay_after: Duration::from_secs(2),
            recompute_interval: Duration::from_millis(500),
            higher_is_better: true,
        }
    }
}

impl TopNConfig {
    /// Creates a config for top-n with default settings.
    pub fn top(n: usize) -> Self {
        Self {
            n,
            ..Default::default()
        }
    }

    /// Sets the metric extension type.
    pub fn with_metric_type(mut self, ext_type: u64) -> Self {
        self.metric_extension_type = ext_type;
        self
    }

    /// Sets the decay timeout.
    pub fn with_decay(mut self, duration: Duration) -> Self {
        self.decay_after = duration;
        self
    }

    /// Sets the recompute interval.
    pub fn with_recompute_interval(mut self, duration: Duration) -> Self {
        self.recompute_interval = duration;
        self
    }

    /// Sets whether higher metric values are better.
    pub fn higher_is_better(mut self, value: bool) -> Self {
        self.higher_is_better = value;
        self
    }
}

/// Track identifier for metric tracking.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct TrackId {
    pub namespace: TrackNamespace,
    pub name: String,
}

impl TrackId {
    pub fn new(namespace: TrackNamespace, name: String) -> Self {
        Self { namespace, name }
    }
}

/// Metric data for a single track.
#[derive(Debug)]
struct TrackMetric {
    /// Current metric value
    value: u64,

    /// Last update time
    last_update: Instant,

    /// Number of samples received
    sample_count: u64,

    /// Running average (for smoothing)
    average: f64,
}

impl TrackMetric {
    fn new(initial_value: u64) -> Self {
        Self {
            value: initial_value,
            last_update: Instant::now(),
            sample_count: 1,
            average: initial_value as f64,
        }
    }

    fn update(&mut self, value: u64, smoothing_factor: f64) {
        self.value = value;
        self.last_update = Instant::now();
        self.sample_count += 1;

        // Exponential moving average
        self.average = self.average * (1.0 - smoothing_factor) + value as f64 * smoothing_factor;
    }

    fn effective_value(&self, decay_after: Duration) -> f64 {
        let elapsed = self.last_update.elapsed();
        if elapsed < decay_after {
            self.average
        } else {
            // Apply decay
            let decay_time = elapsed - decay_after;
            let decay_factor = (-decay_time.as_secs_f64() / 2.0).exp();
            self.average * decay_factor
        }
    }
}

/// Top-N filter that selects tracks based on a metric value.
pub struct TopNFilter {
    config: TopNConfig,

    /// Metrics for each track
    metrics: RwLock<HashMap<TrackId, TrackMetric>>,

    /// Cached top-n track IDs
    cached_top_n: RwLock<Vec<TrackId>>,

    /// Last time top-n was computed
    last_compute: RwLock<Instant>,

    /// Total objects processed
    objects_processed: AtomicU64,

    /// Objects filtered (not in top-n)
    objects_filtered: AtomicU64,

    /// Whether the filter is enabled
    enabled: RwLock<bool>,
}

impl TopNFilter {
    /// Creates a new Top-N filter with the given configuration.
    pub fn new(config: TopNConfig) -> Self {
        Self {
            config,
            metrics: RwLock::new(HashMap::new()),
            cached_top_n: RwLock::new(Vec::new()),
            last_compute: RwLock::new(Instant::now()),
            objects_processed: AtomicU64::new(0),
            objects_filtered: AtomicU64::new(0),
            enabled: RwLock::new(true),
        }
    }

    /// Creates a Top-N filter with default configuration.
    pub fn default_config(n: usize) -> Self {
        Self::new(TopNConfig::top(n))
    }

    /// Enables the filter.
    pub fn enable(&self) {
        *self.enabled.write() = true;
    }

    /// Disables the filter (all tracks pass through).
    pub fn disable(&self) {
        *self.enabled.write() = false;
    }

    /// Returns true if the filter is enabled.
    pub fn is_enabled(&self) -> bool {
        *self.enabled.read()
    }

    /// Updates the metric for a track based on object extensions.
    ///
    /// Call this for every object that flows through the relay.
    /// Returns the extracted metric value, if any.
    pub fn update_metric(
        &self,
        namespace: &TrackNamespace,
        track_name: &str,
        extensions: &[(u64, u64)],
    ) -> Option<u64> {
        // Extract metric from extensions
        let metric_value = extensions
            .iter()
            .find(|(ext_type, _)| *ext_type == self.config.metric_extension_type)
            .map(|(_, value)| *value)?;

        let track_id = TrackId::new(namespace.clone(), track_name.to_string());

        let mut metrics = self.metrics.write();
        if let Some(metric) = metrics.get_mut(&track_id) {
            metric.update(metric_value, 0.3); // 0.3 smoothing factor
        } else {
            metrics.insert(track_id, TrackMetric::new(metric_value));
        }

        Some(metric_value)
    }

    /// Checks if a track should be forwarded (is in top-n).
    ///
    /// This also triggers recomputation of top-n if needed.
    pub fn should_forward(&self, namespace: &TrackNamespace, track_name: &str) -> bool {
        if !self.is_enabled() {
            return true;
        }

        self.objects_processed.fetch_add(1, Ordering::Relaxed);

        // Check if we need to recompute
        let should_recompute = {
            let last = self.last_compute.read();
            last.elapsed() >= self.config.recompute_interval
        };

        if should_recompute {
            self.recompute_top_n();
        }

        let track_id = TrackId::new(namespace.clone(), track_name.to_string());
        let top_n = self.cached_top_n.read();

        let is_in_top_n = top_n.contains(&track_id);
        if !is_in_top_n {
            self.objects_filtered.fetch_add(1, Ordering::Relaxed);
        }

        is_in_top_n
    }

    /// Forces recomputation of the top-n tracks.
    pub fn recompute_top_n(&self) {
        let metrics = self.metrics.read();

        // Collect all tracks with their effective values
        let mut track_values: Vec<_> = metrics
            .iter()
            .map(|(id, metric)| {
                let value = metric.effective_value(self.config.decay_after);
                (id.clone(), value)
            })
            .collect();

        // Sort by metric value
        if self.config.higher_is_better {
            track_values.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            track_values.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        }

        // Take top N
        let top_n: Vec<_> = track_values
            .into_iter()
            .take(self.config.n)
            .map(|(id, _)| id)
            .collect();

        // Update cache
        *self.cached_top_n.write() = top_n;
        *self.last_compute.write() = Instant::now();
    }

    /// Returns the current top-n tracks.
    pub fn get_top_n(&self) -> Vec<TrackId> {
        self.cached_top_n.read().clone()
    }

    /// Returns the current metric value for a track.
    pub fn get_metric(&self, namespace: &TrackNamespace, track_name: &str) -> Option<f64> {
        let track_id = TrackId::new(namespace.clone(), track_name.to_string());
        let metrics = self.metrics.read();
        metrics
            .get(&track_id)
            .map(|m| m.effective_value(self.config.decay_after))
    }

    /// Returns all track metrics (for debugging/monitoring).
    pub fn all_metrics(&self) -> Vec<(TrackId, f64)> {
        let metrics = self.metrics.read();
        metrics
            .iter()
            .map(|(id, m)| (id.clone(), m.effective_value(self.config.decay_after)))
            .collect()
    }

    /// Returns statistics about the filter.
    pub fn stats(&self) -> TopNStats {
        TopNStats {
            objects_processed: self.objects_processed.load(Ordering::Relaxed),
            objects_filtered: self.objects_filtered.load(Ordering::Relaxed),
            tracks_monitored: self.metrics.read().len(),
            current_top_n: self.cached_top_n.read().len(),
        }
    }

    /// Resets statistics.
    pub fn reset_stats(&self) {
        self.objects_processed.store(0, Ordering::Relaxed);
        self.objects_filtered.store(0, Ordering::Relaxed);
    }

    /// Removes stale tracks that haven't been updated for a while.
    pub fn cleanup_stale(&self, max_age: Duration) {
        let mut metrics = self.metrics.write();
        metrics.retain(|_, metric| metric.last_update.elapsed() < max_age);
    }

    /// Returns the configuration.
    pub fn config(&self) -> &TopNConfig {
        &self.config
    }
}

impl Default for TopNFilter {
    fn default() -> Self {
        Self::new(TopNConfig::default())
    }
}

/// Statistics for the Top-N filter.
#[derive(Debug, Clone)]
pub struct TopNStats {
    pub objects_processed: u64,
    pub objects_filtered: u64,
    pub tracks_monitored: usize,
    pub current_top_n: usize,
}

impl TopNStats {
    /// Returns the filter rate (percentage of objects filtered).
    pub fn filter_rate(&self) -> f64 {
        if self.objects_processed == 0 {
            0.0
        } else {
            self.objects_filtered as f64 / self.objects_processed as f64 * 100.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns(path: &str) -> TrackNamespace {
        TrackNamespace::from_utf8_path(path)
    }

    #[test]
    fn test_topn_basic() {
        let filter = TopNFilter::default_config(2);

        // Add metrics for 4 tracks
        filter.update_metric(&ns("room/user1"), "audio", &[(0x100, 100)]);
        filter.update_metric(&ns("room/user2"), "audio", &[(0x100, 50)]);
        filter.update_metric(&ns("room/user3"), "audio", &[(0x100, 200)]);
        filter.update_metric(&ns("room/user4"), "audio", &[(0x100, 75)]);

        filter.recompute_top_n();

        let top = filter.get_top_n();
        assert_eq!(top.len(), 2);

        // user3 (200) and user1 (100) should be in top 2
        assert!(filter.should_forward(&ns("room/user3"), "audio"));
        assert!(filter.should_forward(&ns("room/user1"), "audio"));
        assert!(!filter.should_forward(&ns("room/user2"), "audio"));
        assert!(!filter.should_forward(&ns("room/user4"), "audio"));
    }

    #[test]
    fn test_topn_metric_extraction() {
        let filter = TopNFilter::new(TopNConfig::top(3).with_metric_type(0x200));

        // Should extract metric from correct extension type
        let result = filter.update_metric(&ns("room/user1"), "audio", &[(0x100, 999), (0x200, 42)]);
        assert_eq!(result, Some(42));

        // Should return None if metric extension not present
        let result = filter.update_metric(&ns("room/user2"), "audio", &[(0x100, 999)]);
        assert_eq!(result, None);
    }

    #[test]
    fn test_topn_disabled() {
        let filter = TopNFilter::default_config(1);
        filter.disable();

        // Should forward everything when disabled
        assert!(filter.should_forward(&ns("room/any"), "audio"));
    }

    #[test]
    fn test_topn_metric_update() {
        let filter = TopNFilter::default_config(2);

        // Initial metric
        filter.update_metric(&ns("room/user1"), "audio", &[(0x100, 100)]);
        let metric1 = filter.get_metric(&ns("room/user1"), "audio").unwrap();

        // Update with new value - should smooth
        filter.update_metric(&ns("room/user1"), "audio", &[(0x100, 200)]);
        let metric2 = filter.get_metric(&ns("room/user1"), "audio").unwrap();

        // Metric should have increased but not jumped to 200 immediately
        assert!(metric2 > metric1);
        assert!(metric2 < 200.0);
    }

    #[test]
    fn test_topn_stats() {
        let filter = TopNFilter::default_config(1);

        filter.update_metric(&ns("room/user1"), "audio", &[(0x100, 100)]);
        filter.update_metric(&ns("room/user2"), "audio", &[(0x100, 50)]);
        filter.recompute_top_n();

        // Process some objects
        filter.should_forward(&ns("room/user1"), "audio"); // In top-n
        filter.should_forward(&ns("room/user2"), "audio"); // Not in top-n

        let stats = filter.stats();
        assert_eq!(stats.objects_processed, 2);
        assert_eq!(stats.objects_filtered, 1);
        assert_eq!(stats.tracks_monitored, 2);
        assert_eq!(stats.current_top_n, 1);
    }
}
