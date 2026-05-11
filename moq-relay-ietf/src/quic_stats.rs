//! QUIC Connection Statistics Provider for DTS.
//!
//! This module provides a mechanism to extract and report QUIC connection
//! statistics (RTT, CWND, pacing rate) for bandwidth estimation in DTS.
//!
//! ## Architecture
//!
//! Since the web_transport::Session abstracts away the underlying QUIC connection,
//! we need to extract stats at the point where we have access to the raw quinn::Connection.
//! This happens in the relay's connection acceptance flow.
//!
//! ```text
//! ┌──────────────────────┐       ┌──────────────────────┐
//! │  moq-native-ietf     │       │   moq-relay-ietf     │
//! │  (quinn::Connection) │──────▶│   (DtsService)       │
//! │                      │ stats │                      │
//! │  Server::accept()    │       │   BandwidthEstimator │
//! └──────────────────────┘       └──────────────────────┘
//! ```

use std::time::Duration;
use tokio::sync::mpsc;

/// QUIC path statistics relevant for DTS bandwidth estimation
#[derive(Clone, Debug)]
pub struct QuicPathStats {
    /// Session ID (for routing to the correct DTS subscriber state)
    pub session_id: u64,
    /// Round-trip time
    pub rtt: Duration,
    /// Congestion window in bytes
    pub cwnd: u64,
    /// Number of congestion events (for trend detection)
    pub congestion_events: u64,
    /// Lost packets count
    pub lost_packets: u64,
    /// Total UDP bytes transmitted
    pub udp_tx_bytes: u64,
}

impl QuicPathStats {
    /// Estimate bandwidth in kbps from CWND and RTT
    /// Using: bandwidth = cwnd * 8 * 1_000_000 / rtt_us / 1000 = cwnd * 8000 / rtt_us
    pub fn estimated_bandwidth_kbps(&self) -> u64 {
        let rtt_us = self.rtt.as_micros() as u64;
        if rtt_us == 0 {
            return 0;
        }
        ((self.cwnd * 8000) / rtt_us).min(10_000_000) // Cap at 10 Gbps
    }
}

/// Sender for QUIC stats updates
pub type QuicStatsSender = mpsc::UnboundedSender<QuicPathStats>;

/// Receiver for QUIC stats updates
pub type QuicStatsReceiver = mpsc::UnboundedReceiver<QuicPathStats>;

/// Create a channel for QUIC stats reporting
pub fn quic_stats_channel() -> (QuicStatsSender, QuicStatsReceiver) {
    mpsc::unbounded_channel()
}

/// QUIC Stats Reporter - holds a sender to report stats
#[derive(Clone)]
pub struct QuicStatsReporter {
    sender: QuicStatsSender,
    session_id: u64,
}

impl QuicStatsReporter {
    pub fn new(sender: QuicStatsSender, session_id: u64) -> Self {
        Self { sender, session_id }
    }

    /// Report path stats from a quinn::Connection
    /// Call this periodically (e.g., every 500ms) from where you have connection access
    #[cfg(feature = "quinn-stats")]
    pub fn report_from_connection(&self, conn: &quinn::Connection) {
        let stats = conn.stats();
        let path = stats.path;

        let quic_stats = QuicPathStats {
            session_id: self.session_id,
            rtt: path.rtt,
            cwnd: path.cwnd,
            congestion_events: path.congestion_events,
            lost_packets: path.lost_packets,
        };

        // Best-effort send - don't block if receiver is slow
        let _ = self.sender.send(quic_stats);
    }

    /// Report stats directly (for testing or when raw values are available)
    pub fn report(&self, rtt: Duration, cwnd: u64, congestion_events: u64, lost_packets: u64, udp_tx_bytes: u64) {
        let quic_stats = QuicPathStats {
            session_id: self.session_id,
            rtt,
            cwnd,
            congestion_events,
            lost_packets,
            udp_tx_bytes,
        };

        let _ = self.sender.send(quic_stats);
    }
}

use std::collections::HashMap;
use std::time::Instant;

/// Per-session state for tracking send rate and smoothed bandwidth
struct SessionSendState {
    last_sent_bytes: u64,
    last_update: Instant,
    send_rate_kbps: Option<u64>,
    /// EWMA smoothed send rate for stability
    smoothed_send_rate_kbps: Option<f64>,
    /// EWMA smoothed effective bandwidth (final output)
    smoothed_effective_bw_kbps: Option<f64>,
    /// Time when we last detected congestion (high RTT)
    last_congestion_time: Option<Instant>,
    /// Bandwidth at last congestion event - ceiling for recovery
    congestion_bandwidth_kbps: Option<u64>,
}

/// QUIC Stats Consumer - processes stats updates and feeds them to DtsService
pub struct QuicStatsConsumer {
    receiver: QuicStatsReceiver,
    dts_service: crate::DtsServiceHandle,
    session_states: HashMap<u64, SessionSendState>,
}

impl QuicStatsConsumer {
    pub fn new(receiver: QuicStatsReceiver, dts_service: crate::DtsServiceHandle) -> Self {
        Self {
            receiver,
            dts_service,
            session_states: HashMap::new(),
        }
    }

    /// Run the consumer loop - should be spawned as a background task
    pub async fn run(mut self) {
        while let Some(stats) = self.receiver.recv().await {
            // Convert QUIC stats to bandwidth estimate and update DTS service
            // Use microseconds instead of milliseconds to handle sub-ms RTT (like localhost)
            let rtt_us = stats.rtt.as_micros() as u64;
            if rtt_us > 0 {
                // Calculate cwnd-based bandwidth in kbps: cwnd (bytes) * 8 (bits) / rtt (seconds) / 1000 (to kbps)
                let cwnd_bandwidth_kbps = ((stats.cwnd * 8000) / rtt_us).min(10_000_000);

                // Calculate actual send rate from sent_bytes delta
                let now = Instant::now();
                let state = self.session_states.entry(stats.session_id).or_insert(SessionSendState {
                    last_sent_bytes: stats.udp_tx_bytes,
                    last_update: now,
                    send_rate_kbps: None,
                    smoothed_send_rate_kbps: None,
                    smoothed_effective_bw_kbps: None,
                    last_congestion_time: None,
                    congestion_bandwidth_kbps: None,
                });

                let elapsed_secs = now.duration_since(state.last_update).as_secs_f64();
                let raw_send_rate = if elapsed_secs > 0.1 {
                    // Calculate send rate: (delta_bytes * 8) / elapsed_secs / 1000
                    let delta_bytes = stats.udp_tx_bytes.saturating_sub(state.last_sent_bytes);
                    let rate = ((delta_bytes as f64 * 8.0) / elapsed_secs / 1000.0) as u64;

                    state.last_sent_bytes = stats.udp_tx_bytes;
                    state.last_update = now;
                    state.send_rate_kbps = Some(rate);
                    Some(rate)
                } else {
                    state.send_rate_kbps
                };

                // Smooth the send rate with EWMA (alpha=0.2)
                // Only update when we have actual data flowing (rate > 0)
                let smoothed_send_rate = if let Some(rate) = raw_send_rate {
                    if rate > 0 {
                        let alpha = 0.2;
                        let smoothed = match state.smoothed_send_rate_kbps {
                            Some(prev) => prev * (1.0 - alpha) + rate as f64 * alpha,
                            None => rate as f64,
                        };
                        state.smoothed_send_rate_kbps = Some(smoothed);
                        smoothed as u64
                    } else {
                        // No data flowing - keep previous estimate, don't decay
                        state.smoothed_send_rate_kbps.unwrap_or(0.0) as u64
                    }
                } else {
                    state.smoothed_send_rate_kbps.unwrap_or(0.0) as u64
                };

                // Detect congestion: RTT > 50ms AND we're actively sending significant data (>500 kbps)
                // Only sessions pushing real traffic can detect congestion meaningfully
                let is_actively_sending = smoothed_send_rate > 500;
                let is_congested = rtt_us > 50_000 && is_actively_sending;

                // Track congestion events - only if we're actively sending
                if is_congested {
                    state.last_congestion_time = Some(now);
                    // Remember the bandwidth ceiling when congestion hit
                    if let Some(current_bw) = state.smoothed_effective_bw_kbps {
                        state.congestion_bandwidth_kbps = Some(current_bw as u64);
                    }
                }

                // Check if we're in "recovery" period (within 2 seconds of congestion)
                let in_recovery = is_actively_sending && state.last_congestion_time
                    .map(|t| t.elapsed().as_secs() < 2)
                    .unwrap_or(false);

                // Calculate raw bandwidth estimate
                let raw_effective_bw = if is_congested {
                    // Active congestion: use send_rate with small headroom (20%)
                    // Don't multiply by 2, that's too optimistic when congested
                    (smoothed_send_rate * 6 / 5).min(cwnd_bandwidth_kbps)
                } else if in_recovery {
                    // Recovery period: don't trust cwnd, cap at congestion ceiling
                    let ceiling = state.congestion_bandwidth_kbps.unwrap_or(cwnd_bandwidth_kbps);
                    cwnd_bandwidth_kbps.min(ceiling).min(20_000)
                } else {
                    // Normal: trust cwnd (this includes low-traffic sessions)
                    cwnd_bandwidth_kbps.min(20_000)
                };

                // Apply EWMA smoothing on effective bandwidth
                // Use higher alpha for faster response (~5s total switch time)
                let alpha = 0.4;
                let effective_bw = {
                    let smoothed = match state.smoothed_effective_bw_kbps {
                        Some(prev) => prev * (1.0 - alpha) + raw_effective_bw as f64 * alpha,
                        None => raw_effective_bw as f64,
                    };
                    state.smoothed_effective_bw_kbps = Some(smoothed);
                    smoothed as u64
                };

                // Update DTS with the smoothed effective bandwidth
                let change = self.dts_service.set_bandwidth(stats.session_id, effective_bw);

                // Get the final bandwidth estimate
                let final_bw = self.dts_service.get_bandwidth_kbps(stats.session_id);

                log::info!(
                    "DTS_EVENT: cwnd_bw={} kbps, raw_send={} kbps, smooth_send={} kbps, final_bw={} kbps for session {} (rtt={}us)",
                    cwnd_bandwidth_kbps,
                    raw_send_rate.unwrap_or(0),
                    smoothed_send_rate,
                    final_bw,
                    stats.session_id,
                    rtt_us
                );

                // Log any track switches
                for sel in &change.newly_selected {
                    log::info!(
                        "DTS_SWITCH: track {}/{} now selected for session {} (bw={} kbps)",
                        sel.track.namespace, sel.track.track_name, stats.session_id, final_bw
                    );
                }
                for (set_id, track) in &change.deselected {
                    log::info!(
                        "DTS_SWITCH: track {}/{} deselected (set {}) for session {}",
                        track.namespace, track.track_name, set_id, stats.session_id
                    );
                }
                for (new_sel, old_track) in &change.switched {
                    log::info!(
                        "DTS_SWITCH: switched from {}/{} to {}/{} for session {} (bw={} kbps)",
                        old_track.namespace, old_track.track_name,
                        new_sel.track.namespace, new_sel.track.track_name,
                        stats.session_id, final_bw
                    );
                }
            }
        }

        log::debug!("QUIC stats consumer shutting down");
    }
}

/// Helper to spawn a periodic stats reporter task
/// This can be called from where you have access to the quinn::Connection
pub fn spawn_periodic_stats_reporter(
    _reporter: QuicStatsReporter,
    interval: Duration,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    // In a real implementation, you'd call reporter.report_from_connection()
                    // For now, this is a placeholder showing the pattern
                }
                _ = shutdown.recv() => {
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_bandwidth_estimation() {
        let stats = QuicPathStats {
            session_id: 1,
            rtt: Duration::from_millis(50),
            cwnd: 100_000, // 100KB
            congestion_events: 0,
            lost_packets: 0,
            udp_tx_bytes: 0,
        };

        // bandwidth = cwnd * 8 / rtt_ms = 100000 * 8 / 50 = 16000 kbps
        assert_eq!(stats.estimated_bandwidth_kbps(), 16000);
    }

    #[test]
    fn test_bandwidth_estimation_zero_rtt() {
        let stats = QuicPathStats {
            session_id: 1,
            rtt: Duration::ZERO,
            cwnd: 100_000,
            congestion_events: 0,
            lost_packets: 0,
            udp_tx_bytes: 0,
        };

        // Should not panic, returns 0
        assert_eq!(stats.estimated_bandwidth_kbps(), 0);
    }

    #[tokio::test]
    async fn test_stats_channel() {
        let (sender, mut receiver) = quic_stats_channel();

        let reporter = QuicStatsReporter::new(sender, 123);
        reporter.report(Duration::from_millis(20), 50_000, 0, 0, 10_000);

        let stats = receiver.recv().await.unwrap();
        assert_eq!(stats.session_id, 123);
        assert_eq!(stats.rtt, Duration::from_millis(20));
        assert_eq!(stats.cwnd, 50_000);
    }
}
