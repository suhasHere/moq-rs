//! Filter pipeline configuration and CLI arguments.

use clap::Parser;

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
}

impl Default for FilterArgs {
    fn default() -> Self {
        Self {
            track_ext_filt: true,
            track_filt: true,
            object_filt: true,
            filter_stats: false,
            filter_stats_interval: 60,
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
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            track_ext_enabled: true,
            track_enabled: true,
            object_enabled: true,
            stats_enabled: false,
            stats_interval_secs: 60,
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
        }
    }

    /// Returns true if any filter stage is enabled.
    pub fn any_enabled(&self) -> bool {
        self.track_ext_enabled || self.track_enabled || self.object_enabled
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
}
