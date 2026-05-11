//! Bandwidth estimation for DTS track selection.
//!
//! Provides two complementary approaches:
//! 1. QUIC congestion controller stats (pacing rate, cwnd)
//! 2. Throughput measurement from actual data sent
//!
//! The relay uses these estimates to select appropriate tracks in switching sets.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Bandwidth estimate for a downstream subscriber
#[derive(Clone, Debug)]
pub struct BandwidthEstimate {
    /// Estimated available bandwidth in kbps
    pub bandwidth_kbps: u64,
    /// Source of the estimate
    pub source: BandwidthSource,
    /// When this estimate was last updated
    pub last_update: Instant,
}

/// Source of bandwidth estimation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandwidthSource {
    /// From QUIC congestion controller (pacing rate or cwnd/RTT)
    QuicCongestion,
    /// From measuring actual throughput
    ThroughputMeasurement,
    /// Combined/smoothed estimate
    Combined,
    /// Default/initial value
    Default,
}

/// Configuration for bandwidth estimation
#[derive(Clone, Debug)]
pub struct BandwidthEstimatorConfig {
    /// Window for throughput measurement (how far back to look)
    pub measurement_window: Duration,
    /// How often to sample throughput
    pub sample_interval: Duration,
    /// Smoothing factor for EWMA (0-1, higher = more responsive)
    pub ewma_alpha: f64,
    /// Default bandwidth if no estimate available (kbps)
    pub default_bandwidth_kbps: u64,
    /// Minimum time between estimate updates
    pub min_update_interval: Duration,
}

impl Default for BandwidthEstimatorConfig {
    fn default() -> Self {
        Self {
            measurement_window: Duration::from_secs(5),
            sample_interval: Duration::from_millis(100),
            ewma_alpha: 0.3,
            default_bandwidth_kbps: 0, // No default - must be measured
            min_update_interval: Duration::from_millis(100),
        }
    }
}

/// Per-subscriber throughput measurement state
struct ThroughputState {
    /// Bytes sent in recent samples (timestamp, bytes)
    samples: Vec<(Instant, u64)>,
    /// Running total bytes for current sample
    current_bytes: u64,
    /// Last sample timestamp
    last_sample: Instant,
    /// EWMA of throughput in kbps
    ewma_kbps: Option<f64>,
}

impl ThroughputState {
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(64),
            current_bytes: 0,
            last_sample: Instant::now(),
            ewma_kbps: None,
        }
    }

    fn record_bytes(&mut self, bytes: u64, config: &BandwidthEstimatorConfig) {
        let now = Instant::now();
        self.current_bytes += bytes;

        // Check if we should take a sample
        if now.duration_since(self.last_sample) >= config.sample_interval {
            let elapsed_secs = now.duration_since(self.last_sample).as_secs_f64();
            if elapsed_secs > 0.0 {
                // Calculate throughput for this sample in kbps
                let kbps = (self.current_bytes as f64 * 8.0 / 1000.0) / elapsed_secs;

                // Update EWMA
                self.ewma_kbps = Some(match self.ewma_kbps {
                    Some(prev) => prev * (1.0 - config.ewma_alpha) + kbps * config.ewma_alpha,
                    None => kbps,
                });

                // Record sample
                self.samples.push((now, self.current_bytes));
                self.current_bytes = 0;
                self.last_sample = now;

                // Trim old samples
                let cutoff = now - config.measurement_window;
                self.samples.retain(|(t, _)| *t > cutoff);
            }
        }
    }

    fn get_throughput_kbps(&self) -> Option<u64> {
        self.ewma_kbps.map(|v| v.round() as u64)
    }
}

/// Bandwidth estimator for all downstream subscribers
pub struct BandwidthEstimator {
    /// Per-subscriber state
    subscribers: RwLock<HashMap<u64, SubscriberBandwidthState>>,
    /// Configuration
    config: BandwidthEstimatorConfig,
}

struct SubscriberBandwidthState {
    /// QUIC-based estimate (from congestion controller)
    quic_estimate_kbps: Option<u64>,
    /// Throughput measurement state
    throughput: ThroughputState,
    /// Current combined estimate
    estimate: BandwidthEstimate,
}

impl SubscriberBandwidthState {
    fn new(default_kbps: u64) -> Self {
        Self {
            quic_estimate_kbps: None,
            throughput: ThroughputState::new(),
            estimate: BandwidthEstimate {
                bandwidth_kbps: default_kbps,
                source: BandwidthSource::Default,
                last_update: Instant::now(),
            },
        }
    }
}

impl BandwidthEstimator {
    pub fn new() -> Self {
        Self::with_config(BandwidthEstimatorConfig::default())
    }

    pub fn with_config(config: BandwidthEstimatorConfig) -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Register a new subscriber with default bandwidth
    pub fn register_subscriber(&self, session_id: u64) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers
            .entry(session_id)
            .or_insert_with(|| SubscriberBandwidthState::new(self.config.default_bandwidth_kbps));
    }

    /// Remove a subscriber
    pub fn remove_subscriber(&self, session_id: u64) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.remove(&session_id);
    }

    /// Update QUIC-based bandwidth estimate (call periodically or on congestion events)
    ///
    /// # Arguments
    /// * `session_id` - Subscriber session ID
    /// * `pacing_rate_bps` - QUIC pacing rate in bits per second (from BBR/Cubic)
    /// * `cwnd_bytes` - Congestion window in bytes
    /// * `rtt_ms` - Smoothed RTT in milliseconds
    pub fn update_quic_estimate(
        &self,
        session_id: u64,
        pacing_rate_bps: Option<u64>,
        cwnd_bytes: u64,
        rtt_ms: u64,
    ) {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = match subscribers.get_mut(&session_id) {
            Some(s) => s,
            None => return,
        };

        // Prefer pacing rate if available (more accurate for BBR)
        // Fall back to cwnd/RTT estimate
        let estimate_kbps = if let Some(pacing) = pacing_rate_bps {
            pacing / 1000 // bps to kbps
        } else if rtt_ms > 0 {
            // BDP-based estimate: cwnd * 8 / RTT
            (cwnd_bytes * 8 * 1000) / (rtt_ms * 1000) // bytes to kbps
        } else {
            return;
        };

        state.quic_estimate_kbps = Some(estimate_kbps);
        self.update_combined_estimate(state);
    }

    /// Record bytes sent to a subscriber (for throughput measurement)
    pub fn record_bytes_sent(&self, session_id: u64, bytes: u64) {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = match subscribers.get_mut(&session_id) {
            Some(s) => s,
            None => return,
        };

        state.throughput.record_bytes(bytes, &self.config);
        self.update_combined_estimate(state);
    }

    /// Get current bandwidth estimate for a subscriber
    pub fn get_estimate(&self, session_id: u64) -> Option<BandwidthEstimate> {
        let subscribers = self.subscribers.read().unwrap();
        subscribers.get(&session_id).map(|s| s.estimate.clone())
    }

    /// Get bandwidth in kbps for a subscriber (convenience method)
    pub fn get_bandwidth_kbps(&self, session_id: u64) -> u64 {
        self.get_estimate(session_id)
            .map(|e| e.bandwidth_kbps)
            .unwrap_or(self.config.default_bandwidth_kbps)
    }

    /// Manually set bandwidth for testing/override
    pub fn set_bandwidth(&self, session_id: u64, bandwidth_kbps: u64) {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = subscribers
            .entry(session_id)
            .or_insert_with(|| SubscriberBandwidthState::new(bandwidth_kbps));

        state.estimate = BandwidthEstimate {
            bandwidth_kbps,
            source: BandwidthSource::Combined,
            last_update: Instant::now(),
        };
    }

    fn update_combined_estimate(&self, state: &mut SubscriberBandwidthState) {
        let now = Instant::now();

        // Don't update too frequently
        if now.duration_since(state.estimate.last_update) < self.config.min_update_interval {
            return;
        }

        let quic_kbps = state.quic_estimate_kbps;

        // Use QUIC estimate only for now.
        // Throughput measurement causes feedback loops because it measures what we send,
        // not available bandwidth - when we send lower bitrate, throughput drops,
        // which triggers more downshifts.
        let (bandwidth_kbps, source) = match quic_kbps {
            Some(q) => (q, BandwidthSource::QuicCongestion),
            None => return, // Keep existing estimate
        };

        state.estimate = BandwidthEstimate {
            bandwidth_kbps,
            source,
            last_update: now,
        };
    }
}

impl Default for BandwidthEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe handle
pub type BandwidthEstimatorHandle = Arc<BandwidthEstimator>;

/// Helper to extract bandwidth stats from a quinn Connection
pub struct QuicBandwidthStats {
    pub pacing_rate_bps: Option<u64>,
    pub cwnd_bytes: u64,
    pub rtt_ms: u64,
}

impl QuicBandwidthStats {
    /// Extract stats from quinn PathStats
    pub fn from_path_stats(cwnd: u64, rtt: Duration, pacing_rate: Option<u64>) -> Self {
        Self {
            pacing_rate_bps: pacing_rate,
            cwnd_bytes: cwnd,
            rtt_ms: rtt.as_millis() as u64,
        }
    }

    /// Convert to estimated bandwidth in kbps
    pub fn estimated_bandwidth_kbps(&self) -> u64 {
        if let Some(pacing) = self.pacing_rate_bps {
            pacing / 1000
        } else if self.rtt_ms > 0 {
            (self.cwnd_bytes * 8 * 1000) / (self.rtt_ms * 1000)
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_estimation() {
        let config = BandwidthEstimatorConfig {
            min_update_interval: Duration::from_millis(0), // Disable for testing
            ..Default::default()
        };
        let estimator = BandwidthEstimator::with_config(config);
        let session_id = 1;

        estimator.register_subscriber(session_id);

        // Initially uses default
        let estimate = estimator.get_estimate(session_id).unwrap();
        assert_eq!(estimate.source, BandwidthSource::Default);

        // Update with QUIC stats
        estimator.update_quic_estimate(session_id, Some(5_000_000), 100_000, 50);

        let estimate = estimator.get_estimate(session_id).unwrap();
        assert_eq!(estimate.bandwidth_kbps, 5000); // 5 Mbps
        assert_eq!(estimate.source, BandwidthSource::QuicCongestion);
    }

    #[test]
    fn test_throughput_measurement() {
        let config = BandwidthEstimatorConfig {
            sample_interval: Duration::from_millis(10),
            min_update_interval: Duration::from_millis(1),
            ..Default::default()
        };
        let estimator = BandwidthEstimator::with_config(config);
        let session_id = 1;

        estimator.register_subscriber(session_id);

        // Simulate sending data
        for _ in 0..10 {
            estimator.record_bytes_sent(session_id, 10_000); // 10KB
            std::thread::sleep(Duration::from_millis(10));
        }

        let estimate = estimator.get_estimate(session_id).unwrap();
        // Should have some throughput estimate now
        assert!(estimate.bandwidth_kbps > 0);
    }

    #[test]
    fn test_manual_override() {
        let estimator = BandwidthEstimator::new();
        let session_id = 1;

        estimator.set_bandwidth(session_id, 2000);

        let estimate = estimator.get_estimate(session_id).unwrap();
        assert_eq!(estimate.bandwidth_kbps, 2000);
    }

    #[test]
    fn test_quic_stats_conversion() {
        // With pacing rate
        let stats = QuicBandwidthStats::from_path_stats(
            100_000,                      // cwnd
            Duration::from_millis(50),    // rtt
            Some(10_000_000),             // 10 Mbps pacing
        );
        assert_eq!(stats.estimated_bandwidth_kbps(), 10000);

        // Without pacing rate (cwnd/RTT based)
        let stats = QuicBandwidthStats::from_path_stats(
            100_000,                      // cwnd = 100KB
            Duration::from_millis(100),   // rtt = 100ms
            None,
        );
        // BDP = 100KB * 8 / 0.1s = 8 Mbps
        assert_eq!(stats.estimated_bandwidth_kbps(), 8000);
    }

    #[test]
    fn test_conservative_combined_estimate() {
        let config = BandwidthEstimatorConfig {
            min_update_interval: Duration::from_millis(0),
            sample_interval: Duration::from_millis(1),
            ..Default::default()
        };
        let estimator = BandwidthEstimator::with_config(config);
        let session_id = 1;

        estimator.register_subscriber(session_id);

        // Set QUIC estimate high
        estimator.update_quic_estimate(session_id, Some(10_000_000), 0, 0);

        // But throughput measurement is lower - simulate this by setting manually
        // In reality, record_bytes_sent would build up the measurement
        {
            let mut subscribers = estimator.subscribers.write().unwrap();
            let state = subscribers.get_mut(&session_id).unwrap();
            state.throughput.ewma_kbps = Some(3000.0);
        }

        // Force recalculation
        estimator.update_quic_estimate(session_id, Some(10_000_000), 0, 0);

        // Combined should use minimum (conservative)
        let estimate = estimator.get_estimate(session_id).unwrap();
        assert_eq!(estimate.bandwidth_kbps, 3000);
        assert_eq!(estimate.source, BandwidthSource::Combined);
    }
}
