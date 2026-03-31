//! Filter pipeline configuration and CLI arguments.

use clap::Parser;
use std::time::Duration;

use super::TopNConfig;

/// Command-line arguments for filter configuration.
#[derive(Parser, Debug, Clone)]
pub struct FilterArgs {
    /// Enable track extension filtering stage.
    #[arg(long, default_value = "true")]
    pub track_ext_filt: bool,

    /// Enable track filtering stage.
    #[arg(long, default_value = "true")]
    pub track_filt: bool,

    /// Enable object filtering stage.
    #[arg(long, default_value = "true")]
    pub object_filt: bool,

    /// Enable filter statistics collection.
    /// Note: Has approximately 5% performance overhead when enabled.
    #[arg(long, default_value = "false")]
    pub filter_stats: bool,

    /// Statistics reporting interval in seconds.
    /// Only used when --filter-stats is enabled.
    #[arg(long, default_value = "60")]
    pub filter_stats_interval: u64,

    /// Enable Top-N filtering based on object extension metrics.
    /// When enabled, only the top N tracks (by metric value) are forwarded.
    #[arg(long, default_value = "false")]
    pub topn_enabled: bool,

    /// Number of top tracks to forward (requires --topn-enabled).
    #[arg(long, default_value = "3")]
    pub topn_count: usize,

    /// Extension header type that carries the metric value (hex, e.g., 0x100).
    #[arg(long, default_value = "256")]
    pub topn_metric_type: u64,

    /// Decay timeout in milliseconds - how long before inactive track metrics decay.
    #[arg(long, default_value = "2000")]
    pub topn_decay_ms: u64,

    /// Recompute interval in milliseconds - how often to recalculate top-n.
    #[arg(long, default_value = "500")]
    pub topn_recompute_ms: u64,
}

impl Default for FilterArgs {
    fn default() -> Self {
        Self {
            track_ext_filt: true,
            track_filt: true,
            object_filt: true,
            filter_stats: false,
            filter_stats_interval: 60,
            topn_enabled: false,
            topn_count: 3,
            topn_metric_type: 0x100,
            topn_decay_ms: 2000,
            topn_recompute_ms: 500,
        }
    }
}

impl FilterArgs {
    /// Converts CLI arguments to filter configuration.
    pub fn to_config(&self) -> FilterConfig {
        FilterConfig {
            track_ext_enabled: self.track_ext_filt,
            track_enabled: self.track_filt,
            object_enabled: self.object_filt,
            stats_enabled: self.filter_stats,
            stats_interval_secs: self.filter_stats_interval,
            topn_enabled: self.topn_enabled,
            topn_config: self.to_topn_config(),
        }
    }

    /// Converts CLI arguments to TopN configuration.
    pub fn to_topn_config(&self) -> TopNConfig {
        TopNConfig {
            n: self.topn_count,
            metric_extension_type: self.topn_metric_type,
            decay_after: Duration::from_millis(self.topn_decay_ms),
            recompute_interval: Duration::from_millis(self.topn_recompute_ms),
            higher_is_better: true,
        }
    }
}

/// Runtime configuration for the filter pipeline.
#[derive(Clone, Debug)]
pub struct FilterConfig {
    /// Whether track extension filtering is enabled.
    pub track_ext_enabled: bool,

    /// Whether track filtering is enabled.
    pub track_enabled: bool,

    /// Whether object filtering is enabled.
    pub object_enabled: bool,

    /// Whether statistics collection is enabled.
    pub stats_enabled: bool,

    /// Statistics reporting interval in seconds.
    pub stats_interval_secs: u64,

    /// Whether Top-N filtering is enabled.
    pub topn_enabled: bool,

    /// Top-N filter configuration.
    pub topn_config: TopNConfig,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            track_ext_enabled: true,
            track_enabled: true,
            object_enabled: true,
            stats_enabled: false,
            stats_interval_secs: 60,
            topn_enabled: false,
            topn_config: TopNConfig::default(),
        }
    }
}

impl FilterConfig {
    /// Creates a configuration with all filters enabled.
    pub fn all_enabled() -> Self {
        Self::default()
    }

    /// Creates a configuration with all filters disabled.
    pub fn all_disabled() -> Self {
        Self {
            track_ext_enabled: false,
            track_enabled: false,
            object_enabled: false,
            stats_enabled: false,
            stats_interval_secs: 60,
            topn_enabled: false,
            topn_config: TopNConfig::default(),
        }
    }

    /// Creates a configuration with only object filtering enabled.
    pub fn object_only() -> Self {
        Self {
            track_ext_enabled: false,
            track_enabled: false,
            object_enabled: true,
            stats_enabled: false,
            stats_interval_secs: 60,
            topn_enabled: false,
            topn_config: TopNConfig::default(),
        }
    }

    /// Returns true if any filter stage is enabled.
    pub fn any_enabled(&self) -> bool {
        self.track_ext_enabled || self.track_enabled || self.object_enabled || self.topn_enabled
    }

    /// Enables statistics collection.
    pub fn with_stats(mut self) -> Self {
        self.stats_enabled = true;
        self
    }

    /// Sets the statistics reporting interval.
    pub fn with_stats_interval(mut self, secs: u64) -> Self {
        self.stats_interval_secs = secs;
        self
    }

    /// Enables Top-N filtering with the given count.
    pub fn with_topn(mut self, n: usize) -> Self {
        self.topn_enabled = true;
        self.topn_config.n = n;
        self
    }

    /// Sets the Top-N configuration.
    pub fn with_topn_config(mut self, config: TopNConfig) -> Self {
        self.topn_enabled = true;
        self.topn_config = config;
        self
    }
}
