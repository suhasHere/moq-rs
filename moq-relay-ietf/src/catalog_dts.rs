//! Catalog integration for DTS (Dynamic Track Switching).
//!
//! This module provides automatic detection of switching sets from MoQ catalog metadata.
//! Tracks in the same `alt_group` with the same codec are treated as quality alternatives
//! (switching set members).
//!
//! ## Catalog Format
//!
//! The catalog uses the IETF draft-ietf-moq-catalogformat-01 format:
//! ```json
//! {
//!   "tracks": [
//!     { "name": "video-1080p", "altGroup": 1, "selectionParams": { "bitrate": 4000000 } },
//!     { "name": "video-720p", "altGroup": 1, "selectionParams": { "bitrate": 2000000 } },
//!     { "name": "video-480p", "altGroup": 1, "selectionParams": { "bitrate": 800000 } },
//!     { "name": "audio", "altGroup": 2, "selectionParams": { "samplerate": 48000 } }
//!   ]
//! }
//! ```
//!
//! Tracks with the same `altGroup` form a switching set. The `bitrate` field
//! determines the throughput threshold for selection.

use moq_catalog::Root;
use moq_transport::message::SwitchingSetAssignment;
use std::collections::HashMap;

/// Extracted DTS switching set from catalog
#[derive(Debug, Clone)]
pub struct CatalogSwitchingSet {
    /// The alt_group ID from the catalog
    pub alt_group: u16,
    /// Tracks in this switching set, sorted by bitrate descending
    pub tracks: Vec<CatalogTrack>,
}

/// A track within a switching set
#[derive(Debug, Clone)]
pub struct CatalogTrack {
    /// Track name from catalog
    pub name: String,
    /// Bitrate in kbps (derived from catalog bitrate in bps)
    pub throughput_kbps: u64,
    /// Original bitrate in bps
    pub bitrate_bps: u32,
}

/// Parse a catalog and extract switching sets for DTS
pub fn extract_switching_sets(catalog: &Root) -> Vec<CatalogSwitchingSet> {
    // Group tracks by alt_group
    let mut groups: HashMap<u16, Vec<CatalogTrack>> = HashMap::new();

    for track in &catalog.tracks {
        if let Some(alt_group) = track.alt_group {
            // Only include tracks with bitrate (video/audio with defined quality)
            if let Some(bitrate) = track.selection_params.bitrate {
                let catalog_track = CatalogTrack {
                    name: track.name.clone(),
                    throughput_kbps: (bitrate / 1000) as u64,
                    bitrate_bps: bitrate,
                };

                groups.entry(alt_group).or_default().push(catalog_track);
            }
        }
    }

    // Convert to switching sets
    let mut sets: Vec<CatalogSwitchingSet> = groups
        .into_iter()
        .filter(|(_, tracks)| tracks.len() > 1) // Only include groups with multiple tracks
        .map(|(alt_group, mut tracks)| {
            // Sort by throughput descending (highest quality first)
            tracks.sort_by(|a, b| b.throughput_kbps.cmp(&a.throughput_kbps));

            CatalogSwitchingSet { alt_group, tracks }
        })
        .collect();

    // Sort sets by alt_group for consistency
    sets.sort_by_key(|s| s.alt_group);

    sets
}

/// Generate SWITCHING-SET-ASSIGNMENT parameters for all tracks in a catalog
///
/// Returns a map of track_name -> SwitchingSetAssignment
pub fn generate_dts_params(catalog: &Root) -> HashMap<String, SwitchingSetAssignment> {
    let sets = extract_switching_sets(catalog);
    let mut params = HashMap::new();

    for set in sets {
        let track_count = set.tracks.len();

        for (i, track) in set.tracks.into_iter().enumerate() {
            // Fraction: equal weight for all tracks in set (could be customized)
            let fraction = 10;

            // Rank: use alt_group as rank (lower alt_group = higher priority)
            let rank = set.alt_group as u8;

            // Activate: only the last track activates the set
            let activate = i == track_count - 1;

            let assignment = SwitchingSetAssignment::new(set.alt_group as u64, track.throughput_kbps)
                .with_fraction(fraction)
                .with_rank(rank)
                .with_activate(activate);

            params.insert(track.name, assignment);
        }
    }

    params
}

/// Parse a JSON catalog string and extract DTS parameters
pub fn parse_catalog_for_dts(catalog_json: &str) -> Result<HashMap<String, SwitchingSetAssignment>, serde_json::Error> {
    let catalog: Root = serde_json::from_str(catalog_json)?;
    Ok(generate_dts_params(&catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_catalog::{CommonTrackFields, SelectionParam, Track};

    fn make_track(name: &str, alt_group: Option<u16>, bitrate: Option<u32>) -> Track {
        Track {
            name: name.to_string(),
            alt_group,
            selection_params: SelectionParam {
                bitrate,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn make_catalog(tracks: Vec<Track>) -> Root {
        Root {
            version: 1,
            streaming_format: 1,
            streaming_format_version: "1.0".to_string(),
            streaming_delta_updates: false,
            common_track_fields: CommonTrackFields::default(),
            tracks,
        }
    }

    #[test]
    fn test_extract_switching_sets() {
        let catalog = make_catalog(vec![
            make_track("video-1080p", Some(1), Some(4_000_000)),
            make_track("video-720p", Some(1), Some(2_000_000)),
            make_track("video-480p", Some(1), Some(800_000)),
            make_track("audio-high", Some(2), Some(128_000)),
            make_track("audio-low", Some(2), Some(64_000)),
            make_track("data", None, None), // No alt_group, should be ignored
        ]);

        let sets = extract_switching_sets(&catalog);

        assert_eq!(sets.len(), 2);

        // Video set (alt_group 1)
        let video_set = &sets[0];
        assert_eq!(video_set.alt_group, 1);
        assert_eq!(video_set.tracks.len(), 3);
        assert_eq!(video_set.tracks[0].name, "video-1080p");
        assert_eq!(video_set.tracks[0].throughput_kbps, 4000);
        assert_eq!(video_set.tracks[1].name, "video-720p");
        assert_eq!(video_set.tracks[2].name, "video-480p");

        // Audio set (alt_group 2)
        let audio_set = &sets[1];
        assert_eq!(audio_set.alt_group, 2);
        assert_eq!(audio_set.tracks.len(), 2);
    }

    #[test]
    fn test_generate_dts_params() {
        let catalog = make_catalog(vec![
            make_track("high", Some(1), Some(3_000_000)),
            make_track("medium", Some(1), Some(1_500_000)),
            make_track("low", Some(1), Some(500_000)),
        ]);

        let params = generate_dts_params(&catalog);

        assert_eq!(params.len(), 3);

        // High quality - first in set, activate=false
        let high = params.get("high").unwrap();
        assert_eq!(high.set_id, 1);
        assert_eq!(high.throughput_kbps, 3000);
        assert_eq!(high.activate, false);

        // Medium - middle, activate=false
        let medium = params.get("medium").unwrap();
        assert_eq!(medium.set_id, 1);
        assert_eq!(medium.throughput_kbps, 1500);
        assert_eq!(medium.activate, false);

        // Low quality - last in set, activate=true
        let low = params.get("low").unwrap();
        assert_eq!(low.set_id, 1);
        assert_eq!(low.throughput_kbps, 500);
        assert_eq!(low.activate, true);
    }

    #[test]
    fn test_single_track_group_ignored() {
        let catalog = make_catalog(vec![
            make_track("only-video", Some(1), Some(2_000_000)),
            make_track("audio", Some(2), Some(128_000)),
        ]);

        let sets = extract_switching_sets(&catalog);

        // Single-track groups should be ignored (not a switching set)
        assert_eq!(sets.len(), 0);
    }

    #[test]
    fn test_parse_catalog_json() {
        let json = r#"{
            "version": 1,
            "streamingFormat": 1,
            "streamingFormatVersion": "1.0",
            "supportsDeltaUpdates": false,
            "commonTrackFields": {},
            "tracks": [
                { "name": "1080p", "altGroup": 1, "selectionParams": { "bitrate": 4000000 } },
                { "name": "720p", "altGroup": 1, "selectionParams": { "bitrate": 2000000 } }
            ]
        }"#;

        let params = parse_catalog_for_dts(json).unwrap();

        assert_eq!(params.len(), 2);
        assert!(params.contains_key("1080p"));
        assert!(params.contains_key("720p"));
    }
}
