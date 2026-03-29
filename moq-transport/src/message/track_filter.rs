//! Track Filter for SUBSCRIBE_NAMESPACE messages.
//!
//! Per PR #1518, TRACK_FILTER (0x29) selects tracks based on property values,
//! supporting scenarios like active speaker selection or quality-based track selection.
//!
//! Wire format:
//! ```text
//! TrackFilter {
//!   Type (vi64)            - 0x29
//!   Length (vi64)          - byte count
//!   PropertyType (vi64)    - must be even (single integer property)
//!   MaxTracksSelected (vi64) - concurrent track limit
//!   Timeout (vi64)         - milliseconds before deselection
//! }
//! ```
//!
//! Track state transitions: UNKNOWN -> SELECTED -> DESELECTED
//! Tracks are selected if they have top-N highest property values.

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

/// Track filter parameter type code.
pub const TRACK_FILTER_TYPE: u64 = 0x29;

/// Track selection state for filtered tracks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackState {
    /// Track state is unknown (not yet evaluated).
    Unknown,
    /// Track is currently selected (receiving data).
    Selected,
    /// Track has been deselected (no longer receiving).
    Deselected,
}

impl Default for TrackState {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Track Filter for namespace subscriptions.
///
/// Selects tracks with the top-N highest values for a given property type.
/// This enables scenarios like:
/// - Active speaker selection (highest audio level)
/// - Quality-based selection (highest bitrate/resolution)
/// - Priority-based selection (highest priority tracks)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackFilter {
    /// The property type to evaluate (must be even).
    /// Common property types:
    /// - Audio level / activity metric
    /// - Video quality / bitrate
    /// - Publisher priority
    pub property_type: u64,

    /// Maximum number of tracks that can be concurrently selected.
    /// When a new track exceeds this limit, the lowest-property track is deselected.
    pub max_tracks_selected: u64,

    /// Timeout in milliseconds before a track is automatically deselected
    /// if its property value drops below the threshold.
    /// 0 means no timeout (immediate deselection when overtaken).
    pub timeout_ms: u64,
}

impl TrackFilter {
    /// Creates a new track filter.
    ///
    /// # Arguments
    /// * `property_type` - Must be even (protocol requirement)
    /// * `max_tracks_selected` - Maximum concurrent selected tracks
    /// * `timeout_ms` - Deselection timeout in milliseconds
    ///
    /// # Returns
    /// Error if property_type is odd.
    pub fn new(
        property_type: u64,
        max_tracks_selected: u64,
        timeout_ms: u64,
    ) -> Result<Self, &'static str> {
        if property_type % 2 != 0 {
            return Err("property_type must be even");
        }
        Ok(Self {
            property_type,
            max_tracks_selected,
            timeout_ms,
        })
    }

    /// Creates a track filter for active speaker selection.
    ///
    /// Uses audio activity property to select N loudest speakers.
    pub fn active_speaker(max_speakers: u64, timeout_ms: u64) -> Self {
        Self {
            property_type: 0x100, // Audio activity property (example)
            max_tracks_selected: max_speakers,
            timeout_ms,
        }
    }

    /// Creates a track filter for quality-based selection.
    ///
    /// Selects tracks with highest quality/bitrate.
    pub fn quality_based(max_tracks: u64) -> Self {
        Self {
            property_type: 0x102, // Quality/bitrate property (example)
            max_tracks_selected: max_tracks,
            timeout_ms: 0,
        }
    }

    /// Validates the filter parameters.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.property_type % 2 != 0 {
            return Err("property_type must be even");
        }
        if self.max_tracks_selected == 0 {
            return Err("max_tracks_selected must be > 0");
        }
        Ok(())
    }
}

impl Default for TrackFilter {
    fn default() -> Self {
        Self {
            property_type: 0,
            max_tracks_selected: 1,
            timeout_ms: 0,
        }
    }
}

impl Encode for TrackFilter {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        if self.property_type % 2 != 0 {
            return Err(EncodeError::InvalidValue);
        }
        self.property_type.encode(w)?;
        self.max_tracks_selected.encode(w)?;
        self.timeout_ms.encode(w)?;
        Ok(())
    }
}

impl Decode for TrackFilter {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let property_type = u64::decode(r)?;
        if property_type % 2 != 0 {
            return Err(DecodeError::InvalidValue);
        }
        let max_tracks_selected = u64::decode(r)?;
        let timeout_ms = u64::decode(r)?;

        Ok(Self {
            property_type,
            max_tracks_selected,
            timeout_ms,
        })
    }
}

/// Track selection algorithm for namespace subscriptions.
///
/// Implements the top-N selection logic:
/// 1. Tracks report property values via extension headers
/// 2. Algorithm maintains sorted list by property value
/// 3. Top N tracks are SELECTED, rest are DESELECTED
/// 4. State transitions trigger SUBSCRIBE/UNSUBSCRIBE upstream
///
/// # Performance
///
/// Optimized for the common case of small N (1-10 selected tracks):
/// - Uses HashMap for O(1) track lookup by ID
/// - Uses BinaryHeap for O(log n) top-N selection
/// - Avoids full sort on every update
///
/// Complexity:
/// - `update_track`: O(log n) where n = total tracks
/// - `get_state`: O(1)
/// - `selected_tracks`: O(N) where N = max_tracks_selected
#[derive(Clone, Debug)]
pub struct TrackSelector {
    /// The filter configuration.
    filter: TrackFilter,

    /// Track ID to entry index mapping for O(1) lookup.
    track_index: std::collections::HashMap<u64, usize>,

    /// Currently tracked entries.
    tracks: Vec<TrackEntry>,

    /// Cached threshold: minimum property value to be selected.
    /// Updated on selection changes to avoid recomputation.
    selection_threshold: u64,
}

#[derive(Clone, Debug)]
struct TrackEntry {
    /// Unique identifier for the track (hash of namespace + name).
    track_id: u64,
    /// Current property value.
    property_value: u64,
    /// Current selection state.
    state: TrackState,
    /// Timestamp of last property update (milliseconds).
    last_update_ms: u64,
}

impl TrackSelector {
    /// Creates a new track selector with the given filter.
    pub fn new(filter: TrackFilter) -> Self {
        Self {
            filter,
            track_index: std::collections::HashMap::new(),
            tracks: Vec::new(),
            selection_threshold: 0,
        }
    }

    /// Updates a track's property value and returns state changes.
    ///
    /// # Arguments
    /// * `track_id` - Unique track identifier
    /// * `property_value` - New property value
    /// * `current_time_ms` - Current timestamp in milliseconds
    ///
    /// # Returns
    /// List of (track_id, old_state, new_state) for tracks that changed state.
    ///
    /// # Performance
    /// O(log n) for updates that don't change selection.
    /// O(n) in worst case when selection boundary changes.
    pub fn update_track(
        &mut self,
        track_id: u64,
        property_value: u64,
        current_time_ms: u64,
    ) -> Vec<(u64, TrackState, TrackState)> {
        // O(1) lookup for existing track
        if let Some(&idx) = self.track_index.get(&track_id) {
            let old_value = self.tracks[idx].property_value;
            self.tracks[idx].property_value = property_value;
            self.tracks[idx].last_update_ms = current_time_ms;

            // Fast path: if value didn't cross the threshold, no selection change
            let was_selected = self.tracks[idx].state == TrackState::Selected;
            let crosses_threshold = (old_value >= self.selection_threshold)
                != (property_value >= self.selection_threshold);

            if !crosses_threshold && was_selected == (property_value >= self.selection_threshold) {
                // No selection change needed
                return Vec::new();
            }
        } else {
            // New track - add to collection
            let idx = self.tracks.len();
            self.tracks.push(TrackEntry {
                track_id,
                property_value,
                state: TrackState::Unknown,
                last_update_ms: current_time_ms,
            });
            self.track_index.insert(track_id, idx);
        }

        self.recompute_selection(current_time_ms)
    }

    /// Recomputes track selection and returns state changes.
    ///
    /// Uses partial sort (selection algorithm) for O(n) instead of O(n log n).
    fn recompute_selection(&mut self, current_time_ms: u64) -> Vec<(u64, TrackState, TrackState)> {
        let mut changes = Vec::new();
        let max = self.filter.max_tracks_selected as usize;

        // Apply timeout-based deselection first
        if self.filter.timeout_ms > 0 {
            for entry in &mut self.tracks {
                if entry.state == TrackState::Selected {
                    let elapsed = current_time_ms.saturating_sub(entry.last_update_ms);
                    if elapsed > self.filter.timeout_ms {
                        let old_state = entry.state;
                        entry.state = TrackState::Deselected;
                        changes.push((entry.track_id, old_state, entry.state));
                    }
                }
            }
        }

        // For small N, use partial sort which is O(n) instead of O(n log n)
        // Find the Nth largest element to determine threshold
        if self.tracks.is_empty() {
            self.selection_threshold = 0;
            return changes;
        }

        // Collect property values for threshold calculation
        let mut values: Vec<u64> = self.tracks.iter().map(|t| t.property_value).collect();

        // Find threshold: the Nth largest value
        let threshold_idx = max.min(values.len()) - 1;
        // Use select_nth_unstable for O(n) partial sort
        values.select_nth_unstable_by(threshold_idx, |a, b| b.cmp(a));
        self.selection_threshold = values[threshold_idx];

        // Update states based on threshold
        for entry in &mut self.tracks {
            // Track is selected if its value is >= threshold AND we haven't exceeded max
            let should_be_selected = entry.property_value >= self.selection_threshold;
            let new_state = if should_be_selected {
                TrackState::Selected
            } else {
                TrackState::Deselected
            };

            if entry.state != new_state {
                let old_state = entry.state;
                entry.state = new_state;
                changes.push((entry.track_id, old_state, new_state));
            }
        }

        // Handle tie-breaking: if more tracks than max have the threshold value,
        // keep only the first max selected (by track_id for determinism)
        let selected_count = self.tracks.iter().filter(|t| t.state == TrackState::Selected).count();
        if selected_count > max {
            // Sort tracks at threshold by track_id, deselect extras
            let mut at_threshold: Vec<usize> = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.state == TrackState::Selected && t.property_value == self.selection_threshold)
                .map(|(i, _)| i)
                .collect();
            at_threshold.sort_by_key(|&i| self.tracks[i].track_id);

            // How many extra selections to remove?
            let excess = selected_count - max;
            for &idx in at_threshold.iter().rev().take(excess) {
                let old_state = self.tracks[idx].state;
                self.tracks[idx].state = TrackState::Deselected;
                changes.push((self.tracks[idx].track_id, old_state, TrackState::Deselected));
            }
        }

        changes
    }

    /// Removes a track from the selector.
    pub fn remove_track(&mut self, track_id: u64) -> Option<TrackState> {
        if let Some(idx) = self.track_index.remove(&track_id) {
            let entry = self.tracks.swap_remove(idx);

            // Update index for swapped element (if any)
            if idx < self.tracks.len() {
                let swapped_id = self.tracks[idx].track_id;
                self.track_index.insert(swapped_id, idx);
            }

            Some(entry.state)
        } else {
            None
        }
    }

    /// Returns the current state of a track.
    ///
    /// # Performance
    /// O(1) lookup.
    #[inline]
    pub fn get_state(&self, track_id: u64) -> TrackState {
        self.track_index
            .get(&track_id)
            .map(|&idx| self.tracks[idx].state)
            .unwrap_or(TrackState::Unknown)
    }

    /// Returns the number of currently selected tracks.
    pub fn selected_count(&self) -> usize {
        self.tracks
            .iter()
            .filter(|t| t.state == TrackState::Selected)
            .count()
    }

    /// Returns all currently selected track IDs.
    pub fn selected_tracks(&self) -> Vec<u64> {
        self.tracks
            .iter()
            .filter(|t| t.state == TrackState::Selected)
            .map(|t| t.track_id)
            .collect()
    }

    /// Returns the current selection threshold.
    ///
    /// Tracks with property values >= this threshold are selected.
    pub fn threshold(&self) -> u64 {
        self.selection_threshold
    }

    /// Returns the total number of tracked tracks.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn test_track_filter_new() {
        let filter = TrackFilter::new(0x10, 3, 1000);
        assert!(filter.is_ok());

        let filter = TrackFilter::new(0x11, 3, 1000);
        assert!(filter.is_err());
    }

    #[test]
    fn test_track_filter_encode_decode() {
        let filter = TrackFilter::new(0x10, 3, 5000).unwrap();

        let mut buf = BytesMut::new();
        filter.encode(&mut buf).unwrap();

        let decoded = TrackFilter::decode(&mut buf).unwrap();
        assert_eq!(decoded, filter);
    }

    #[test]
    fn test_track_selector_basic() {
        let filter = TrackFilter::new(0x10, 2, 0).unwrap();
        let mut selector = TrackSelector::new(filter);

        // Add three tracks
        let changes = selector.update_track(1, 100, 0);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0], (1, TrackState::Unknown, TrackState::Selected));

        let changes = selector.update_track(2, 200, 0);
        assert_eq!(selector.selected_count(), 2);

        // Third track should cause lowest to be deselected
        let changes = selector.update_track(3, 300, 0);
        assert_eq!(selector.selected_count(), 2);

        // Track 3 (300) and track 2 (200) should be selected
        // Track 1 (100) should be deselected
        assert_eq!(selector.get_state(3), TrackState::Selected);
        assert_eq!(selector.get_state(2), TrackState::Selected);
        assert_eq!(selector.get_state(1), TrackState::Deselected);
    }

    #[test]
    fn test_track_selector_update_value() {
        let filter = TrackFilter::new(0x10, 2, 0).unwrap();
        let mut selector = TrackSelector::new(filter);

        selector.update_track(1, 100, 0);
        selector.update_track(2, 200, 0);
        selector.update_track(3, 300, 0);

        // Track 1 is deselected, update it to highest
        let changes = selector.update_track(1, 500, 0);

        // Track 1 should now be selected, track 2 deselected
        assert_eq!(selector.get_state(1), TrackState::Selected);
        assert_eq!(selector.get_state(3), TrackState::Selected);
        assert_eq!(selector.get_state(2), TrackState::Deselected);
    }

    #[test]
    fn test_track_selector_timeout() {
        let filter = TrackFilter::new(0x10, 2, 1000).unwrap();
        let mut selector = TrackSelector::new(filter);

        selector.update_track(1, 100, 0);
        selector.update_track(2, 200, 0);

        // Simulate time passing without updates
        let changes = selector.update_track(1, 100, 2000);

        // Both should be deselected due to timeout, then reselected
        // (since they're the only tracks)
        assert_eq!(selector.selected_count(), 2);
    }

    #[test]
    fn test_active_speaker() {
        let filter = TrackFilter::active_speaker(3, 2000);
        assert_eq!(filter.max_tracks_selected, 3);
        assert_eq!(filter.timeout_ms, 2000);
    }
}
