//! DTS (Dynamic Track Switching) Tracker for ABR track selection.
//!
//! Implements bandwidth-based track selection per draft-wilaw-moq-dts4moq.
//! Each subscriber can have multiple switching sets, and the relay selects
//! exactly one track per set based on available bandwidth.
//!
//! ## Key Concepts
//!
//! - **Switching Set**: Collection of time-aligned tracks at different bitrates
//! - **Throughput Threshold**: Minimum bandwidth (kbps) needed to select a track
//! - **Fraction**: Relative weight for bandwidth allocation (1-10)
//! - **Rank**: Degradation priority (lower = higher priority, protected first)
//!
//! ## Bandwidth Allocation Algorithm
//!
//! 1. Sort sets by rank (ascending - higher priority first)
//! 2. Calculate each set's target: `target = B_total * fraction / sum_F`
//! 3. Allocate: `allocated = min(target, B_remaining)`
//! 4. Select highest-throughput track that fits allocation
//! 5. Subtract selected track's throughput from B_remaining

use moq_transport::coding::TrackNamespace;
use moq_transport::message::SwitchingSetAssignment;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

/// A track within a switching set
#[derive(Clone, Debug)]
pub struct DtsTrack {
    pub namespace: TrackNamespace,
    pub track_name: String,
    pub throughput_kbps: u64,
    pub subscription_id: u64,
}

/// A switching set containing multiple tracks at different bitrates
#[derive(Clone, Debug)]
pub struct SwitchingSet {
    pub set_id: u64,
    pub fraction: u64,
    pub rank: u8,
    /// Tracks sorted by throughput (descending - highest quality first)
    pub tracks: Vec<DtsTrack>,
    /// Currently selected track index (None = no track fits)
    pub selected_track: Option<usize>,
    /// Whether the set is active (all tracks registered)
    pub active: bool,
    /// Last selection update time
    pub last_update: Instant,
}

impl SwitchingSet {
    pub fn new(set_id: u64, fraction: u64, rank: u8) -> Self {
        Self {
            set_id,
            fraction,
            rank,
            tracks: Vec::new(),
            selected_track: None,
            active: false,
            last_update: Instant::now(),
        }
    }

    /// Add a track to this set, maintaining sorted order by throughput descending
    pub fn add_track(&mut self, track: DtsTrack) {
        let pos = self
            .tracks
            .binary_search_by(|t| track.throughput_kbps.cmp(&t.throughput_kbps))
            .unwrap_or_else(|p| p);
        self.tracks.insert(pos, track);
    }

    /// Remove a track by subscription ID
    pub fn remove_track(&mut self, subscription_id: u64) -> bool {
        if let Some(pos) = self.tracks.iter().position(|t| t.subscription_id == subscription_id) {
            self.tracks.remove(pos);
            // Adjust selected_track index if needed
            if let Some(selected) = self.selected_track {
                if pos < selected {
                    self.selected_track = Some(selected - 1);
                } else if pos == selected {
                    self.selected_track = None;
                }
            }
            true
        } else {
            false
        }
    }

    /// Get the currently selected track
    pub fn selected(&self) -> Option<&DtsTrack> {
        self.selected_track.and_then(|idx| self.tracks.get(idx))
    }

    /// Select the best track that fits within the allocated bandwidth
    /// Returns true if selection changed
    pub fn select_for_bandwidth(&mut self, allocated_kbps: u64) -> bool {
        let old_selected = self.selected_track;

        // Find highest-throughput track that fits
        self.selected_track = self
            .tracks
            .iter()
            .enumerate()
            .find(|(_, t)| t.throughput_kbps <= allocated_kbps)
            .map(|(idx, _)| idx);

        self.last_update = Instant::now();
        old_selected != self.selected_track
    }
}

/// Result of track selection
#[derive(Clone, Debug)]
pub struct TrackSelection {
    pub set_id: u64,
    pub track: DtsTrack,
    pub allocated_kbps: u64,
}

/// Result of a bandwidth update
#[derive(Clone, Debug, Default)]
pub struct SelectionChange {
    /// Tracks that were newly selected (start forwarding)
    pub newly_selected: Vec<TrackSelection>,
    /// Tracks that were deselected (stop forwarding)
    pub deselected: Vec<(u64, DtsTrack)>, // (set_id, track)
    /// Tracks that changed within a set (switch quality)
    pub switched: Vec<(TrackSelection, DtsTrack)>, // (new, old)
}

impl SelectionChange {
    pub fn is_empty(&self) -> bool {
        self.newly_selected.is_empty() && self.deselected.is_empty() && self.switched.is_empty()
    }
}

/// Configuration for DTS Tracker
#[derive(Clone, Debug)]
pub struct DtsTrackerConfig {
    /// Minimum time between selection changes (hysteresis)
    pub min_switch_interval_ms: u64,
    /// Bandwidth headroom percentage for upward switches (require this much extra to go up)
    pub upswitch_headroom_pct: u64,
    /// Bandwidth margin percentage for downward switches (only go down if this much below)
    pub downswitch_margin_pct: u64,
    /// Enable event logging for debugging
    pub enable_logging: bool,
}

impl Default for DtsTrackerConfig {
    fn default() -> Self {
        Self {
            min_switch_interval_ms: 2000,  // 2 seconds minimum between switches
            upswitch_headroom_pct: 15,     // Need 15% extra bandwidth to upswitch
            downswitch_margin_pct: 20,     // Only downswitch if 20% below threshold
            enable_logging: false,
        }
    }
}

/// Per-subscriber DTS state
struct SubscriberDtsState {
    /// Switching sets for this subscriber (set_id -> set)
    sets: HashMap<u64, SwitchingSet>,
    /// Current bandwidth estimate in kbps
    bandwidth_kbps: u64,
    /// Last bandwidth update time
    last_bandwidth_update: Instant,
    /// Last time any track switch occurred (for hysteresis)
    last_switch_time: Option<Instant>,
}

impl SubscriberDtsState {
    fn new() -> Self {
        Self {
            sets: HashMap::new(),
            bandwidth_kbps: 0,
            last_bandwidth_update: Instant::now(),
            last_switch_time: None,
        }
    }

    /// Calculate sum of fractions for active sets
    fn sum_fractions(&self) -> u64 {
        self.sets
            .values()
            .filter(|s| s.active)
            .map(|s| s.fraction)
            .sum()
    }
}

/// DTS Tracker manages bandwidth-based track selection for all subscribers
pub struct DtsTracker {
    /// Per-subscriber state (session_id -> state)
    subscribers: RwLock<HashMap<u64, SubscriberDtsState>>,
    /// Configuration
    config: DtsTrackerConfig,
}

impl DtsTracker {
    pub fn new() -> Self {
        Self::with_config(DtsTrackerConfig::default())
    }

    pub fn with_config(config: DtsTrackerConfig) -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// Register a track in a switching set for a subscriber
    pub fn register_track(
        &self,
        session_id: u64,
        subscription_id: u64,
        namespace: TrackNamespace,
        track_name: String,
        assignment: &SwitchingSetAssignment,
    ) {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = subscribers.entry(session_id).or_insert_with(SubscriberDtsState::new);

        let set = state.sets.entry(assignment.set_id).or_insert_with(|| {
            SwitchingSet::new(assignment.set_id, assignment.fraction, assignment.effective_rank())
        });

        // Update set parameters (in case they changed)
        set.fraction = assignment.fraction;
        set.rank = assignment.effective_rank();

        // Add the track
        set.add_track(DtsTrack {
            namespace,
            track_name: track_name.clone(),
            throughput_kbps: assignment.throughput_kbps,
            subscription_id,
        });

        // Activate if requested
        if assignment.activate {
            set.active = true;
        }

        if self.config.enable_logging {
            log::info!(
                "DTS: registered track {} for session {} in set {} (throughput={}kbps, active={})",
                track_name,
                session_id,
                assignment.set_id,
                assignment.throughput_kbps,
                set.active
            );
        }
    }

    /// Remove a track from a switching set
    /// Returns the removed track info and triggers reselection if needed
    pub fn remove_track(&self, session_id: u64, subscription_id: u64) -> Option<(u64, DtsTrack)> {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = subscribers.get_mut(&session_id)?;

        let mut removed_info = None;
        let mut needs_reselect = false;

        for (set_id, set) in state.sets.iter_mut() {
            if let Some(pos) = set.tracks.iter().position(|t| t.subscription_id == subscription_id) {
                let track = set.tracks.remove(pos);
                let was_selected = set.selected_track == Some(pos);

                // Adjust selected index
                if let Some(selected) = set.selected_track {
                    if pos < selected {
                        set.selected_track = Some(selected - 1);
                    } else if pos == selected {
                        set.selected_track = None;
                        needs_reselect = true;
                    }
                }

                if self.config.enable_logging {
                    log::info!(
                        "DTS: removed track {} from session {} set {} (was_selected={})",
                        track.track_name,
                        session_id,
                        set_id,
                        was_selected
                    );
                }

                removed_info = Some((*set_id, track));
                break;
            }
        }

        // Reselect if the removed track was selected
        if needs_reselect && removed_info.is_some() {
            self.select_tracks_internal(state, session_id);
        }

        removed_info
    }

    /// Remove all state for a subscriber
    pub fn remove_subscriber(&self, session_id: u64) {
        let mut subscribers = self.subscribers.write().unwrap();
        subscribers.remove(&session_id);
    }

    /// Update bandwidth estimate for a subscriber and reselect tracks
    pub fn update_bandwidth(&self, session_id: u64, bandwidth_kbps: u64) -> SelectionChange {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = match subscribers.get_mut(&session_id) {
            Some(s) => s,
            None => return SelectionChange::default(),
        };

        let old_bandwidth = state.bandwidth_kbps;
        state.bandwidth_kbps = bandwidth_kbps;
        state.last_bandwidth_update = Instant::now();

        if self.config.enable_logging && old_bandwidth != bandwidth_kbps {
            log::debug!(
                "DTS: bandwidth update for session {}: {}kbps -> {}kbps",
                session_id,
                old_bandwidth,
                bandwidth_kbps
            );
        }

        self.select_tracks_internal(state, session_id)
    }

    /// Activate a switching set (called when activate=true is received)
    pub fn activate_set(&self, session_id: u64, set_id: u64) -> SelectionChange {
        let mut subscribers = self.subscribers.write().unwrap();
        let state = match subscribers.get_mut(&session_id) {
            Some(s) => s,
            None => return SelectionChange::default(),
        };

        if let Some(set) = state.sets.get_mut(&set_id) {
            set.active = true;
            if self.config.enable_logging {
                log::info!("DTS: activated set {} for session {}", set_id, session_id);
            }
        }

        self.select_tracks_internal(state, session_id)
    }

    /// Get current selections for a subscriber
    pub fn get_selections(&self, session_id: u64) -> Vec<TrackSelection> {
        let subscribers = self.subscribers.read().unwrap();
        let state = match subscribers.get(&session_id) {
            Some(s) => s,
            None => return Vec::new(),
        };

        state
            .sets
            .values()
            .filter(|s| s.active)
            .filter_map(|set| {
                set.selected().map(|track| TrackSelection {
                    set_id: set.set_id,
                    track: track.clone(),
                    allocated_kbps: track.throughput_kbps,
                })
            })
            .collect()
    }

    /// Check if a track is currently selected for forwarding
    pub fn is_track_selected(&self, session_id: u64, subscription_id: u64) -> bool {
        let subscribers = self.subscribers.read().unwrap();
        let state = match subscribers.get(&session_id) {
            Some(s) => s,
            None => return false,
        };

        state.sets.values().any(|set| {
            set.selected()
                .map(|t| t.subscription_id == subscription_id)
                .unwrap_or(false)
        })
    }

    /// Check if a track belongs to any switching set
    pub fn is_track_in_switching_set(&self, session_id: u64, subscription_id: u64) -> bool {
        let subscribers = self.subscribers.read().unwrap();
        let state = match subscribers.get(&session_id) {
            Some(s) => s,
            None => return false,
        };

        state
            .sets
            .values()
            .any(|set| set.tracks.iter().any(|t| t.subscription_id == subscription_id))
    }

    /// Get the switching set ID for a track, if any
    pub fn get_track_set_id(&self, session_id: u64, subscription_id: u64) -> Option<u64> {
        let subscribers = self.subscribers.read().unwrap();
        let state = subscribers.get(&session_id)?;

        state
            .sets
            .iter()
            .find(|(_, set)| set.tracks.iter().any(|t| t.subscription_id == subscription_id))
            .map(|(set_id, _)| *set_id)
    }

    /// Check if a track's switching set is active
    pub fn is_set_active_for_track(&self, session_id: u64, subscription_id: u64) -> bool {
        let subscribers = self.subscribers.read().unwrap();
        let state = match subscribers.get(&session_id) {
            Some(s) => s,
            None => return false,
        };

        state
            .sets
            .values()
            .find(|set| set.tracks.iter().any(|t| t.subscription_id == subscription_id))
            .map(|set| set.active)
            .unwrap_or(false)
    }

    /// Check if a switching set is active by set_id
    pub fn is_set_active(&self, session_id: u64, set_id: u64) -> bool {
        let subscribers = self.subscribers.read().unwrap();
        let state = match subscribers.get(&session_id) {
            Some(s) => s,
            None => return false,
        };

        state
            .sets
            .get(&set_id)
            .map(|set| set.active)
            .unwrap_or(false)
    }

    /// Internal track selection algorithm
    fn select_tracks_internal(&self, state: &mut SubscriberDtsState, session_id: u64) -> SelectionChange {
        let mut change = SelectionChange::default();
        let b_total = state.bandwidth_kbps;
        let sum_f = state.sum_fractions();

        if sum_f == 0 || b_total == 0 {
            return change;
        }

        // Time-based hysteresis: don't switch too frequently
        let min_interval = std::time::Duration::from_millis(self.config.min_switch_interval_ms);
        let can_switch = match state.last_switch_time {
            Some(last) => last.elapsed() >= min_interval,
            None => true, // First selection is always allowed
        };

        // Collect active sets and sort by rank (ascending = higher priority first)
        let mut active_sets: Vec<&mut SwitchingSet> = state
            .sets
            .values_mut()
            .filter(|s| s.active && !s.tracks.is_empty())
            .collect();
        active_sets.sort_by_key(|s| s.rank);

        let mut b_remaining = b_total;

        for set in active_sets {
            let old_selected = set.selected().cloned();

            // Calculate target allocation
            let target = b_total * set.fraction / sum_f;
            let allocated = target.min(b_remaining);

            // Apply bandwidth hysteresis to prevent oscillation
            // - For upswitch: require headroom (e.g., 20% above target track's bitrate)
            // - For downswitch: require margin (e.g., 15% below current track's bitrate)
            let is_initial_selection = old_selected.is_none();

            let should_consider_switch = if is_initial_selection {
                true
            } else if let Some(ref current) = old_selected {
                let current_bitrate = current.throughput_kbps;

                // Check if bandwidth is significantly below current track (downswitch condition)
                let downswitch_threshold = current_bitrate.saturating_sub(
                    current_bitrate * self.config.downswitch_margin_pct / 100
                );
                let should_downswitch = allocated < downswitch_threshold;

                // Check if bandwidth is significantly above next higher track (upswitch condition)
                // Find next higher track's bitrate
                let next_higher_bitrate = set.tracks.iter()
                    .filter(|t| t.throughput_kbps > current_bitrate)
                    .map(|t| t.throughput_kbps)
                    .min();
                let should_upswitch = if let Some(higher) = next_higher_bitrate {
                    let upswitch_threshold = higher + (higher * self.config.upswitch_headroom_pct / 100);
                    allocated >= upswitch_threshold
                } else {
                    false // Already at highest
                };

                should_downswitch || should_upswitch
            } else {
                true
            };

            // Select best track - but only if we can switch (time + bandwidth hysteresis)
            let changed = if (can_switch && should_consider_switch) || is_initial_selection {
                set.select_for_bandwidth(allocated)
            } else {
                false
            };

            if changed {
                let new_selected = set.selected().cloned();

                match (old_selected, new_selected) {
                    (None, Some(new)) => {
                        change.newly_selected.push(TrackSelection {
                            set_id: set.set_id,
                            track: new,
                            allocated_kbps: allocated,
                        });
                    }
                    (Some(old), None) => {
                        change.deselected.push((set.set_id, old));
                    }
                    (Some(old), Some(new)) if old.subscription_id != new.subscription_id => {
                        change.switched.push((
                            TrackSelection {
                                set_id: set.set_id,
                                track: new,
                                allocated_kbps: allocated,
                            },
                            old,
                        ));
                        // Update last switch time for quality switches
                        state.last_switch_time = Some(Instant::now());
                    }
                    _ => {}
                }
            }

            // Deduct from remaining bandwidth
            if let Some(track) = set.selected() {
                b_remaining = b_remaining.saturating_sub(track.throughput_kbps);
            }
        }

        if self.config.enable_logging && !change.is_empty() {
            log::info!(
                "DTS: selection changed for session {}: {} newly selected, {} deselected, {} switched",
                session_id,
                change.newly_selected.len(),
                change.deselected.len(),
                change.switched.len()
            );
        }

        change
    }

    /// Get statistics for debugging
    pub fn stats(&self, session_id: u64) -> Option<DtsStats> {
        let subscribers = self.subscribers.read().unwrap();
        let state = subscribers.get(&session_id)?;

        Some(DtsStats {
            bandwidth_kbps: state.bandwidth_kbps,
            num_sets: state.sets.len(),
            num_active_sets: state.sets.values().filter(|s| s.active).count(),
            num_tracks: state.sets.values().map(|s| s.tracks.len()).sum(),
            selections: state
                .sets
                .values()
                .filter_map(|s| {
                    s.selected().map(|t| (s.set_id, t.track_name.clone(), t.throughput_kbps))
                })
                .collect(),
        })
    }
}

impl Default for DtsTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Debug statistics
#[derive(Clone, Debug)]
pub struct DtsStats {
    pub bandwidth_kbps: u64,
    pub num_sets: usize,
    pub num_active_sets: usize,
    pub num_tracks: usize,
    /// (set_id, track_name, throughput_kbps)
    pub selections: Vec<(u64, String, u64)>,
}

/// Thread-safe handle to DTS Tracker
pub type DtsTrackerHandle = Arc<DtsTracker>;

#[cfg(test)]
mod tests {
    use super::*;
    use moq_transport::coding::TrackNamespace;

    fn ns(path: &str) -> TrackNamespace {
        TrackNamespace::from_utf8_path(path)
    }

    fn assignment(set_id: u64, throughput: u64, fraction: u64, activate: bool) -> SwitchingSetAssignment {
        SwitchingSetAssignment {
            set_id,
            throughput_kbps: throughput,
            fraction,
            activate,
            rank: None,
        }
    }

    #[test]
    fn test_basic_selection() {
        let tracker = DtsTracker::new();
        let session_id = 1;

        // Register 3 tracks at different bitrates
        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "1080p".to_string(),
            &assignment(1, 3000, 10, false),
        );
        tracker.register_track(
            session_id,
            101,
            ns("video"),
            "720p".to_string(),
            &assignment(1, 1500, 10, false),
        );
        tracker.register_track(
            session_id,
            102,
            ns("video"),
            "480p".to_string(),
            &assignment(1, 800, 10, true), // activate
        );

        // With 5000 kbps, should select 1080p (3000)
        let change = tracker.update_bandwidth(session_id, 5000);
        assert_eq!(change.newly_selected.len(), 1);
        assert_eq!(change.newly_selected[0].track.track_name, "1080p");

        // With 2000 kbps, should switch to 720p (1500)
        let change = tracker.update_bandwidth(session_id, 2000);
        assert_eq!(change.switched.len(), 1);
        assert_eq!(change.switched[0].0.track.track_name, "720p");
        assert_eq!(change.switched[0].1.track_name, "1080p");

        // With 1000 kbps, should switch to 480p (800)
        let change = tracker.update_bandwidth(session_id, 1000);
        assert_eq!(change.switched.len(), 1);
        assert_eq!(change.switched[0].0.track.track_name, "480p");
    }

    #[test]
    fn test_multiple_sets_with_rank() {
        let config = DtsTrackerConfig {
            upswitch_headroom_pct: 0, // Disable for predictable testing
            ..Default::default()
        };
        let tracker = DtsTracker::with_config(config);
        let session_id = 1;

        // Set 1: Main video (rank=1, fraction=6) - higher priority
        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "main-1080p".to_string(),
            &SwitchingSetAssignment {
                set_id: 1,
                throughput_kbps: 3000,
                fraction: 6,
                activate: false,
                rank: Some(1),
            },
        );
        tracker.register_track(
            session_id,
            101,
            ns("video"),
            "main-480p".to_string(),
            &SwitchingSetAssignment {
                set_id: 1,
                throughput_kbps: 800,
                fraction: 6,
                activate: true,
                rank: Some(1),
            },
        );

        // Set 2: Secondary view (rank=2, fraction=4) - lower priority
        tracker.register_track(
            session_id,
            200,
            ns("video"),
            "secondary-720p".to_string(),
            &SwitchingSetAssignment {
                set_id: 2,
                throughput_kbps: 1500,
                fraction: 4,
                activate: false,
                rank: Some(2),
            },
        );
        tracker.register_track(
            session_id,
            201,
            ns("video"),
            "secondary-480p".to_string(),
            &SwitchingSetAssignment {
                set_id: 2,
                throughput_kbps: 800,
                fraction: 4,
                activate: true,
                rank: Some(2),
            },
        );

        // With 5000 kbps: both should get good quality
        // sum_f = 10, set1 target = 5000*6/10 = 3000, set2 target = 5000*4/10 = 2000
        let change = tracker.update_bandwidth(session_id, 5000);
        assert_eq!(change.newly_selected.len(), 2);

        let selections = tracker.get_selections(session_id);
        assert_eq!(selections.len(), 2);

        // Find selections by set
        let set1 = selections.iter().find(|s| s.set_id == 1).unwrap();
        let set2 = selections.iter().find(|s| s.set_id == 2).unwrap();

        assert_eq!(set1.track.track_name, "main-1080p"); // 3000 kbps fits in 3000 target
        assert_eq!(set2.track.track_name, "secondary-720p"); // 1500 kbps fits in 2000 target

        // With 2000 kbps: high-priority set gets protected
        // sum_f = 10, set1 target = 2000*6/10 = 1200, set2 target = 2000*4/10 = 800
        // But rank matters: set1 (rank=1) processed first with full 2000 available
        let _ = tracker.update_bandwidth(session_id, 2000);

        let selections = tracker.get_selections(session_id);
        let set1 = selections.iter().find(|s| s.set_id == 1).unwrap();
        let set2 = selections.iter().find(|s| s.set_id == 2).unwrap();

        // Set1 gets 1200 target but has 2000 available -> can't fit 3000, selects 800 (main-480p)
        // Actually: target=1200, allocated=min(1200,2000)=1200, 800 fits
        assert_eq!(set1.track.track_name, "main-480p");
        // Set2 gets remaining: 2000 - 800 = 1200, target=800, allocated=min(800,1200)=800
        assert_eq!(set2.track.track_name, "secondary-480p");
    }

    #[test]
    fn test_track_removal() {
        let tracker = DtsTracker::new();
        let session_id = 1;

        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "1080p".to_string(),
            &assignment(1, 3000, 10, false),
        );
        tracker.register_track(
            session_id,
            101,
            ns("video"),
            "720p".to_string(),
            &assignment(1, 1500, 10, true),
        );

        tracker.update_bandwidth(session_id, 5000);
        assert!(tracker.is_track_selected(session_id, 100)); // 1080p selected

        // Remove the 1080p track
        let removed = tracker.remove_track(session_id, 100);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().1.track_name, "1080p");

        // 720p should now be selected
        let selections = tracker.get_selections(session_id);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].track.track_name, "720p");
    }

    #[test]
    fn test_inactive_set_ignored() {
        let tracker = DtsTracker::new();
        let session_id = 1;

        // Register but don't activate
        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "1080p".to_string(),
            &assignment(1, 3000, 10, false),
        );

        tracker.update_bandwidth(session_id, 5000);

        // No selections because set is not active
        let selections = tracker.get_selections(session_id);
        assert!(selections.is_empty());

        // Activate
        tracker.activate_set(session_id, 1);

        let selections = tracker.get_selections(session_id);
        assert_eq!(selections.len(), 1);
    }

    #[test]
    fn test_is_track_in_switching_set() {
        let tracker = DtsTracker::new();
        let session_id = 1;

        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "1080p".to_string(),
            &assignment(1, 3000, 10, true),
        );

        assert!(tracker.is_track_in_switching_set(session_id, 100));
        assert!(!tracker.is_track_in_switching_set(session_id, 999));
        assert!(!tracker.is_track_in_switching_set(999, 100));
    }

    #[test]
    fn test_bandwidth_zero() {
        let tracker = DtsTracker::new();
        let session_id = 1;

        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "480p".to_string(),
            &assignment(1, 800, 10, true),
        );

        // Zero bandwidth should select nothing
        let change = tracker.update_bandwidth(session_id, 0);
        assert!(change.newly_selected.is_empty());

        let selections = tracker.get_selections(session_id);
        assert!(selections.is_empty());
    }

    #[test]
    fn test_get_track_set_id() {
        let tracker = DtsTracker::new();
        let session_id = 1;

        tracker.register_track(
            session_id,
            100,
            ns("video"),
            "1080p".to_string(),
            &assignment(1, 3000, 10, true),
        );
        tracker.register_track(
            session_id,
            200,
            ns("audio"),
            "high".to_string(),
            &assignment(2, 128, 5, true),
        );

        assert_eq!(tracker.get_track_set_id(session_id, 100), Some(1));
        assert_eq!(tracker.get_track_set_id(session_id, 200), Some(2));
        assert_eq!(tracker.get_track_set_id(session_id, 999), None);
    }
}
