//! DTS (Dynamic Track Switching) Service for relay integration.
//!
//! This module provides a high-level service that coordinates:
//! - DTS Tracker (track selection based on bandwidth)
//! - Bandwidth Estimator (QUIC stats + throughput measurement)
//! - Group-boundary switching logic
//!
//! ## Usage
//!
//! 1. Create a DtsService and add it to the relay
//! 2. When a SUBSCRIBE with SWITCHING-SET-ASSIGNMENT arrives, register the track
//! 3. Periodically update bandwidth estimates
//! 4. Use is_forwarding() to check if objects should be forwarded

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use moq_transport::coding::TrackNamespace;
use moq_transport::message::DtsParams;

use crate::bandwidth::{BandwidthEstimator, BandwidthEstimatorConfig};
use crate::dts_tracker::{DtsTracker, DtsTrackerConfig, SelectionChange, TrackSelection};

/// Configuration for DTS Service
#[derive(Clone, Debug)]
pub struct DtsServiceConfig {
    /// DTS tracker configuration
    pub tracker: DtsTrackerConfig,
    /// Bandwidth estimator configuration
    pub bandwidth: BandwidthEstimatorConfig,
    /// How often to update bandwidth and reselect tracks (ms)
    pub bandwidth_update_interval_ms: u64,
    /// Enable debug logging
    pub enable_logging: bool,
}

impl Default for DtsServiceConfig {
    fn default() -> Self {
        Self {
            tracker: DtsTrackerConfig::default(),
            bandwidth: BandwidthEstimatorConfig::default(),
            bandwidth_update_interval_ms: 500,
            enable_logging: false,
        }
    }
}

/// Tracks the current group being forwarded for a switching set
/// Used for group-boundary switching
#[derive(Clone, Debug)]
struct GroupState {
    /// Current group being forwarded
    current_group: Option<u64>,
    /// Track that owns the current group
    current_track_subscription_id: Option<u64>,
    /// Pending switch to this track (waiting for group boundary)
    pending_switch: Option<u64>,
    /// Buffer for first group if no prior data exists
    buffered_group: Option<u64>,
}

impl Default for GroupState {
    fn default() -> Self {
        Self {
            current_group: None,
            current_track_subscription_id: None,
            pending_switch: None,
            buffered_group: None,
        }
    }
}

/// Per-subscriber DTS state
struct SubscriberDtsServiceState {
    /// Group state per switching set
    group_states: HashMap<u64, GroupState>,
    /// Last bandwidth update time
    last_bandwidth_update: Instant,
}

impl SubscriberDtsServiceState {
    fn new() -> Self {
        Self {
            group_states: HashMap::new(),
            last_bandwidth_update: Instant::now(),
        }
    }
}

/// DTS Service coordinates bandwidth estimation and track selection
pub struct DtsService {
    /// Track selection tracker
    tracker: DtsTracker,
    /// Bandwidth estimator
    bandwidth: BandwidthEstimator,
    /// Per-subscriber state for group-boundary switching
    subscribers: RwLock<HashMap<u64, SubscriberDtsServiceState>>,
    /// Configuration
    config: DtsServiceConfig,
}

impl DtsService {
    pub fn new() -> Self {
        Self::with_config(DtsServiceConfig::default())
    }

    pub fn with_config(config: DtsServiceConfig) -> Self {
        Self {
            tracker: DtsTracker::with_config(config.tracker.clone()),
            bandwidth: BandwidthEstimator::with_config(config.bandwidth.clone()),
            subscribers: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Register a subscriber session
    pub fn register_subscriber(&self, session_id: u64) {
        self.bandwidth.register_subscriber(session_id);
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.entry(session_id).or_insert_with(SubscriberDtsServiceState::new);
    }

    /// Remove a subscriber session
    pub fn remove_subscriber(&self, session_id: u64) {
        self.tracker.remove_subscriber(session_id);
        self.bandwidth.remove_subscriber(session_id);
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.remove(&session_id);
    }

    /// Register a track from a SUBSCRIBE message with SWITCHING-SET-ASSIGNMENT
    ///
    /// Call this when processing a SUBSCRIBE that contains the DTS parameter.
    /// Returns true if this was a DTS subscription, false otherwise.
    pub fn register_track_from_subscribe(
        &self,
        session_id: u64,
        subscription_id: u64,
        namespace: &TrackNamespace,
        track_name: &str,
        params: &moq_transport::coding::KeyValuePairs,
    ) -> bool {
        if let Some(assignment) = params.get_switching_set_assignment() {
            self.tracker.register_track(
                session_id,
                subscription_id,
                namespace.clone(),
                track_name.to_string(),
                &assignment,
            );

            // Initialize group state for this set if not exists
            {
                let mut subscribers = self.subscribers.write().unwrap();
                let state = subscribers.entry(session_id).or_insert_with(SubscriberDtsServiceState::new);
                state.group_states.entry(assignment.set_id).or_default();
            }

            // Check if this set is already active (e.g., resubscribing to a track)
            let set_already_active = self.tracker.is_set_active(session_id, assignment.set_id);

            // If activate=true, activate the set
            if assignment.activate {
                self.tracker.activate_set(session_id, assignment.set_id);
            }

            // Trigger re-selection if:
            // 1. This is the activating track (activate=true), OR
            // 2. The set is already active (new track joining active set should compete)
            if assignment.activate || set_already_active {
                let bandwidth = self.bandwidth.get_bandwidth_kbps(session_id);
                log::info!(
                    "DTS_EVENT: bandwidth={} kbps for session {} (set_active={}, activate={})",
                    bandwidth, session_id, set_already_active, assignment.activate
                );
                let change = self.tracker.update_bandwidth(session_id, bandwidth);

                // Log selection results
                for sel in &change.newly_selected {
                    log::info!(
                        "DTS_EVENT: track {}/{} selected=true (bandwidth={} kbps)",
                        sel.track.namespace, sel.track.track_name, bandwidth
                    );
                }
                for (set_id, track) in &change.deselected {
                    log::info!(
                        "DTS_EVENT: track {}/{} selected=false (set {})",
                        track.namespace, track.track_name, set_id
                    );
                }
            }

            if self.config.enable_logging {
                log::info!(
                    "DTS: registered track {}/{} for session {} in set {} (throughput={}kbps, activate={})",
                    namespace,
                    track_name,
                    session_id,
                    assignment.set_id,
                    assignment.throughput_kbps,
                    assignment.activate
                );
            }

            true
        } else {
            false
        }
    }

    /// Remove a track when unsubscribed
    /// Returns the selection change if a new track was selected to replace the removed one
    pub fn remove_track(&self, session_id: u64, subscription_id: u64) -> SelectionChange {
        log::info!(
            "DTS_DEBUG: remove_track called for session={} subscription_id={}",
            session_id, subscription_id
        );

        // Get the set_id before removal to clear group state
        let set_id = self.tracker.get_track_set_id(session_id, subscription_id);

        // Remove from tracker - this triggers reselection if needed
        let removed = self.tracker.remove_track(session_id, subscription_id);

        // Clear group state for this set if track was removed
        if let (Some(set_id), Some(_)) = (set_id, &removed) {
            let mut subscribers = self.subscribers.write().unwrap();
            if let Some(state) = subscribers.get_mut(&session_id) {
                if let Some(group_state) = state.group_states.get_mut(&set_id) {
                    // Reset group state to allow new selection to take over
                    if group_state.current_track_subscription_id == Some(subscription_id) {
                        group_state.current_track_subscription_id = None;
                        group_state.current_group = None;
                        group_state.pending_switch = None;
                    }
                }
            }
        }

        // Return selection change info
        if removed.is_some() {
            // Get current selections to build change info
            let selections = self.tracker.get_selections(session_id);
            if let Some(sel) = selections.into_iter().find(|s| set_id == Some(s.set_id)) {
                log::info!(
                    "DTS: track removed, new selection: {}/{} for session {}",
                    sel.track.namespace, sel.track.track_name, session_id
                );
                SelectionChange {
                    newly_selected: vec![sel],
                    deselected: vec![],
                    switched: vec![],
                }
            } else {
                SelectionChange::default()
            }
        } else {
            SelectionChange::default()
        }
    }

    /// Update bandwidth from QUIC stats
    ///
    /// Call this periodically or when congestion state changes.
    pub fn update_bandwidth_from_quic(
        &self,
        session_id: u64,
        pacing_rate_bps: Option<u64>,
        cwnd_bytes: u64,
        rtt_ms: u64,
    ) -> SelectionChange {
        self.bandwidth.update_quic_estimate(session_id, pacing_rate_bps, cwnd_bytes, rtt_ms);
        let bandwidth = self.bandwidth.get_bandwidth_kbps(session_id);
        self.tracker.update_bandwidth(session_id, bandwidth)
    }

    /// Record bytes sent (for throughput measurement)
    pub fn record_bytes_sent(&self, session_id: u64, bytes: u64) {
        self.bandwidth.record_bytes_sent(session_id, bytes);
    }

    /// Set bandwidth manually (for testing)
    pub fn set_bandwidth(&self, session_id: u64, bandwidth_kbps: u64) -> SelectionChange {
        self.bandwidth.set_bandwidth(session_id, bandwidth_kbps);
        self.tracker.update_bandwidth(session_id, bandwidth_kbps)
    }

    /// Check if an object should be forwarded to a subscriber
    ///
    /// This is the main filtering function called during object forwarding.
    /// It handles:
    /// 1. Checking if track is in a switching set
    /// 2. If so, checking if it's the selected track
    /// 3. Group-boundary switching logic
    ///
    /// Returns true if the object should be forwarded.
    pub fn should_forward_object(
        &self,
        session_id: u64,
        subscription_id: u64,
        group_id: u64,
        _object_id: u64,
    ) -> bool {
        // Check if this track is in a switching set
        let set_id = match self.tracker.get_track_set_id(session_id, subscription_id) {
            Some(id) => id,
            None => {
                log::debug!(
                    "DTS: subscription_id={} not in any switching set for session={}, forwarding",
                    subscription_id, session_id
                );
                return true; // Not in a switching set, always forward
            }
        };

        // Check if this track is currently selected
        let is_selected = self.tracker.is_track_selected(session_id, subscription_id);

        log::debug!(
            "DTS: should_forward session={} sub_id={} group={} set_id={} is_selected={}",
            session_id, subscription_id, group_id, set_id, is_selected
        );

        // Get group state for switching logic
        let mut subscribers = self.subscribers.write().unwrap();
        let state = match subscribers.get_mut(&session_id) {
            Some(s) => s,
            None => return is_selected, // No state, fall back to selection
        };

        let group_state = state.group_states.entry(set_id).or_default();

        // Group-boundary switching logic
        match (group_state.current_track_subscription_id, is_selected) {
            (None, true) => {
                // First object from selected track - start forwarding
                group_state.current_track_subscription_id = Some(subscription_id);
                group_state.current_group = Some(group_id);
                true
            }
            (None, false) => {
                // Not selected and no current track - buffer if this is first
                if group_state.buffered_group.is_none() {
                    group_state.buffered_group = Some(group_id);
                }
                false
            }
            (Some(current), true) if current == subscription_id => {
                // Same track, update group if new
                if Some(group_id) != group_state.current_group {
                    group_state.current_group = Some(group_id);
                    // Clear pending switch if we got a new group from current track
                    if group_state.pending_switch.is_some() {
                        group_state.pending_switch = None;
                    }
                }
                true
            }
            (Some(current), true) if current != subscription_id => {
                // Different track is now selected - need to switch at group boundary
                // Set pending switch and wait for new group from new track
                group_state.pending_switch = Some(subscription_id);

                // If this is a new group from the newly selected track, switch now
                if Some(group_id) != group_state.current_group {
                    group_state.current_track_subscription_id = Some(subscription_id);
                    group_state.current_group = Some(group_id);
                    group_state.pending_switch = None;
                    true
                } else {
                    // Same group, don't switch yet (continue old track)
                    false
                }
            }
            (Some(current), false) if current == subscription_id => {
                // This track is no longer selected
                // Continue forwarding until the new track's group arrives
                if group_state.pending_switch.is_none() {
                    // No pending switch yet, keep forwarding current
                    true
                } else {
                    // There's a pending switch, check if we should stop
                    // Continue until new group from new track
                    true
                }
            }
            (Some(_), false) => {
                // Not selected and not current - don't forward
                false
            }
            _ => is_selected,
        }
    }

    /// Check if a track is currently selected (without group-boundary logic)
    pub fn is_track_selected(&self, session_id: u64, subscription_id: u64) -> bool {
        self.tracker.is_track_selected(session_id, subscription_id)
    }

    /// Check if a track belongs to any switching set
    pub fn is_track_in_switching_set(&self, session_id: u64, subscription_id: u64) -> bool {
        self.tracker.is_track_in_switching_set(session_id, subscription_id)
    }

    /// Check if a track's switching set is active
    pub fn is_set_active_for_track(&self, session_id: u64, subscription_id: u64) -> bool {
        self.tracker.is_set_active_for_track(session_id, subscription_id)
    }

    /// Get current selections for a subscriber
    pub fn get_selections(&self, session_id: u64) -> Vec<TrackSelection> {
        self.tracker.get_selections(session_id)
    }

    /// Get current bandwidth estimate for a subscriber
    pub fn get_bandwidth_kbps(&self, session_id: u64) -> u64 {
        self.bandwidth.get_bandwidth_kbps(session_id)
    }
}

impl Default for DtsService {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe handle
pub type DtsServiceHandle = Arc<DtsService>;

#[cfg(test)]
mod tests {
    use super::*;
    use moq_transport::coding::KeyValuePairs;
    use moq_transport::message::SwitchingSetAssignment;

    fn ns(path: &str) -> TrackNamespace {
        TrackNamespace::from_utf8_path(path)
    }

    fn make_params(set_id: u64, throughput: u64, fraction: u64, activate: bool) -> KeyValuePairs {
        let mut params = KeyValuePairs::new();
        let assignment = SwitchingSetAssignment::new(set_id, throughput)
            .with_fraction(fraction)
            .with_activate(activate);
        params.set_switching_set_assignment(&assignment);
        params
    }

    #[test]
    fn test_basic_dts_flow() {
        let service = DtsService::new();
        let session_id = 1;

        service.register_subscriber(session_id);

        // Register 3 tracks at different bitrates
        let params_1080p = make_params(1, 3000, 10, false);
        let params_720p = make_params(1, 1500, 10, false);
        let params_480p = make_params(1, 800, 10, true);

        service.register_track_from_subscribe(session_id, 100, &ns("video"), "1080p", &params_1080p);
        service.register_track_from_subscribe(session_id, 101, &ns("video"), "720p", &params_720p);
        service.register_track_from_subscribe(session_id, 102, &ns("video"), "480p", &params_480p);

        // Set bandwidth high - should select 1080p
        service.set_bandwidth(session_id, 5000);

        assert!(service.is_track_selected(session_id, 100)); // 1080p
        assert!(!service.is_track_selected(session_id, 101));
        assert!(!service.is_track_selected(session_id, 102));

        // Reduce bandwidth - should switch to 720p
        service.set_bandwidth(session_id, 2000);

        assert!(!service.is_track_selected(session_id, 100));
        assert!(service.is_track_selected(session_id, 101)); // 720p
        assert!(!service.is_track_selected(session_id, 102));
    }

    #[test]
    fn test_object_forwarding() {
        let service = DtsService::new();
        let session_id = 1;

        service.register_subscriber(session_id);

        let params_high = make_params(1, 3000, 10, false);
        let params_low = make_params(1, 800, 10, true);

        service.register_track_from_subscribe(session_id, 100, &ns("video"), "high", &params_high);
        service.register_track_from_subscribe(session_id, 101, &ns("video"), "low", &params_low);

        service.set_bandwidth(session_id, 5000);

        // High quality is selected
        assert!(service.should_forward_object(session_id, 100, 1, 0));
        assert!(!service.should_forward_object(session_id, 101, 1, 0));
    }

    #[test]
    fn test_non_dts_track_always_forwards() {
        let service = DtsService::new();
        let session_id = 1;

        service.register_subscriber(session_id);

        // Track without DTS parameter
        let params = KeyValuePairs::new();
        let is_dts = service.register_track_from_subscribe(session_id, 200, &ns("audio"), "stereo", &params);

        assert!(!is_dts);

        // Non-DTS track should always forward
        assert!(service.should_forward_object(session_id, 200, 1, 0));
    }

    #[test]
    fn test_group_boundary_switching() {
        let config = DtsServiceConfig {
            tracker: DtsTrackerConfig {
                upswitch_headroom_pct: 0, // Disable for predictable testing
                ..Default::default()
            },
            ..Default::default()
        };
        let service = DtsService::with_config(config);
        let session_id = 1;

        service.register_subscriber(session_id);

        let params_high = make_params(1, 3000, 10, false);
        let params_low = make_params(1, 800, 10, true);

        service.register_track_from_subscribe(session_id, 100, &ns("video"), "high", &params_high);
        service.register_track_from_subscribe(session_id, 101, &ns("video"), "low", &params_low);

        // Start with high bandwidth - high quality
        service.set_bandwidth(session_id, 5000);

        // Forward first object from high track, group 1
        assert!(service.should_forward_object(session_id, 100, 1, 0));
        assert!(service.should_forward_object(session_id, 100, 1, 1));

        // Reduce bandwidth - low quality now selected
        service.set_bandwidth(session_id, 1000);

        // High track continues forwarding in same group (group boundary not yet reached)
        assert!(service.should_forward_object(session_id, 100, 1, 2));

        // New group from low track - switch happens
        assert!(service.should_forward_object(session_id, 101, 2, 0));

        // High track no longer forwards
        assert!(!service.should_forward_object(session_id, 100, 2, 0));
    }
}
