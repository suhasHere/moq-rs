//! Session Publisher Tracker
//!
//! Tracks which sessions are publishers for self-exclusion support.
//! When a session sends PUBLISH, we record their publisher_id.
//! When the same session sends SUBSCRIBE_NAMESPACE, we use that publisher_id
//! for self-exclusion (they won't see their own tracks in top-N).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::filter::PublisherId;

/// Tracks publisher identity for a session.
///
/// Shared between Consumer (receives PUBLISH) and Producer (handles SUBSCRIBE_NAMESPACE).
/// When a session publishes tracks, their publisher_id is recorded here.
/// When they subscribe, this ID is used for self-exclusion.
#[derive(Clone, Debug)]
pub struct SessionPublisherTracker {
    inner: Arc<SessionPublisherInner>,
}

#[derive(Debug)]
struct SessionPublisherInner {
    /// The publisher_id for this session (0 = not a publisher)
    publisher_id: AtomicU64,

    /// Track aliases published by this session (for debugging)
    published_tracks: RwLock<Vec<u64>>,
}

impl SessionPublisherTracker {
    /// Create a new tracker for a session.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SessionPublisherInner {
                publisher_id: AtomicU64::new(0),
                published_tracks: RwLock::new(Vec::new()),
            }),
        }
    }

    /// Record that this session is publishing a track.
    ///
    /// The first track_alias becomes the session's publisher_id.
    /// Subsequent publishes are tracked but don't change the ID.
    pub fn record_publish(&self, track_alias: u64) {
        // Use first track_alias as publisher_id (CAS to avoid race)
        let _ = self.inner.publisher_id.compare_exchange(
            0,
            track_alias,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );

        // Track all published aliases
        self.inner.published_tracks.write().push(track_alias);

        log::debug!(
            "session recorded as publisher: publisher_id={}, track_alias={}",
            self.get_publisher_id().unwrap_or(0),
            track_alias
        );
    }

    /// Get the publisher_id for this session, if they're a publisher.
    ///
    /// Returns None if the session hasn't published any tracks.
    pub fn get_publisher_id(&self) -> Option<PublisherId> {
        let id = self.inner.publisher_id.load(Ordering::SeqCst);
        if id == 0 {
            None
        } else {
            Some(id)
        }
    }

    /// Check if this session is a publisher.
    pub fn is_publisher(&self) -> bool {
        self.inner.publisher_id.load(Ordering::SeqCst) != 0
    }

    /// Get the number of tracks published by this session.
    pub fn published_track_count(&self) -> usize {
        self.inner.published_tracks.read().len()
    }
}

impl Default for SessionPublisherTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_session_not_publisher() {
        let tracker = SessionPublisherTracker::new();
        assert!(!tracker.is_publisher());
        assert_eq!(tracker.get_publisher_id(), None);
    }

    #[test]
    fn test_record_publish() {
        let tracker = SessionPublisherTracker::new();

        tracker.record_publish(42);
        assert!(tracker.is_publisher());
        assert_eq!(tracker.get_publisher_id(), Some(42));
        assert_eq!(tracker.published_track_count(), 1);
    }

    #[test]
    fn test_first_publish_sets_id() {
        let tracker = SessionPublisherTracker::new();

        tracker.record_publish(100);
        tracker.record_publish(200);
        tracker.record_publish(300);

        // Publisher ID is the first track_alias
        assert_eq!(tracker.get_publisher_id(), Some(100));
        assert_eq!(tracker.published_track_count(), 3);
    }

    #[test]
    fn test_clone_shares_state() {
        let tracker1 = SessionPublisherTracker::new();
        let tracker2 = tracker1.clone();

        tracker1.record_publish(42);

        // Both see the same state
        assert_eq!(tracker2.get_publisher_id(), Some(42));
    }
}
