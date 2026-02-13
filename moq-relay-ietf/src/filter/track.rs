//! Track Filter implementation.
//!
//! Filters tracks at subscription time based on namespace/name and enforces limits.
//! Uses Bloom filter for fast negative lookups with exact matching fallback.

use rustc_hash::FxHashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use moq_transport::coding::TrackNamespace;

use super::FilterStats;

/// Compact track identifier for efficient hashing and comparison.
#[derive(Clone, Debug)]
pub struct TrackIdentifier {
    /// Interned namespace (shared across subscriptions).
    pub namespace: Arc<TrackNamespace>,

    /// Track name.
    pub name: String,

    /// Pre-computed hash for fast comparison.
    hash: u64,
}

impl TrackIdentifier {
    /// Creates a new track identifier.
    pub fn new(namespace: &TrackNamespace, name: &str) -> Self {
        let hash = Self::compute_hash(namespace, name);
        Self {
            namespace: Arc::new(namespace.clone()),
            name: name.to_string(),
            hash,
        }
    }

    /// Creates a track identifier with a shared namespace.
    pub fn with_shared_namespace(namespace: Arc<TrackNamespace>, name: String) -> Self {
        let hash = Self::compute_hash(&namespace, &name);
        Self {
            namespace,
            name,
            hash,
        }
    }

    /// Computes the hash for a namespace/name pair.
    fn compute_hash(namespace: &TrackNamespace, name: &str) -> u64 {
        use rustc_hash::FxHasher;
        let mut hasher = FxHasher::default();
        namespace.hash(&mut hasher);
        name.hash(&mut hasher);
        hasher.finish()
    }

    /// Returns the pre-computed hash.
    #[inline]
    pub fn hash(&self) -> u64 {
        self.hash
    }
}

impl PartialEq for TrackIdentifier {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.name == other.name && *self.namespace == *other.namespace
    }
}

impl Eq for TrackIdentifier {}

impl Hash for TrackIdentifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}

/// Limits for track selection.
#[derive(Clone, Copy, Debug, Default)]
pub struct TrackLimits {
    /// Maximum number of tracks that can be selected (subscribed to).
    /// 0 means unlimited.
    pub max_selected: u64,

    /// Maximum number of tracks that can be deselected (rejected).
    /// 0 means unlimited.
    pub max_deselected: u64,

    /// Maximum time in milliseconds a track can be selected.
    /// 0 means unlimited.
    pub max_time_selected_ms: u64,
}

impl TrackLimits {
    /// Creates unlimited track limits.
    pub const fn unlimited() -> Self {
        Self {
            max_selected: 0,
            max_deselected: 0,
            max_time_selected_ms: 0,
        }
    }

    /// Creates limits with a maximum number of selected tracks.
    pub const fn max_tracks(max: u64) -> Self {
        Self {
            max_selected: max,
            max_deselected: 0,
            max_time_selected_ms: 0,
        }
    }
}

/// Simple Bloom filter for fast negative lookups.
///
/// Uses 3 hash functions and a configurable bit count.
/// False positive rate ≈ (1 - e^(-3n/m))^3 where n = items, m = bits.
struct BloomFilter {
    bits: Vec<u64>,
    num_bits: usize,
}

impl BloomFilter {
    /// Creates a new Bloom filter with the specified number of bits.
    fn new(num_bits: usize) -> Self {
        let num_words = (num_bits + 63) / 64;
        Self {
            bits: vec![0; num_words],
            num_bits,
        }
    }

    /// Creates a Bloom filter sized for the expected number of items.
    /// Uses 10 bits per item for ~1% false positive rate.
    fn for_capacity(expected_items: usize) -> Self {
        let num_bits = (expected_items * 10).max(64);
        Self::new(num_bits)
    }

    /// Inserts a hash into the filter.
    fn insert(&mut self, hash: u64) {
        let h1 = hash;
        let h2 = hash.rotate_left(21);
        let h3 = hash.rotate_left(42);

        self.set_bit(h1 % self.num_bits as u64);
        self.set_bit(h2 % self.num_bits as u64);
        self.set_bit(h3 % self.num_bits as u64);
    }

    /// Checks if a hash might be in the filter.
    fn may_contain(&self, hash: u64) -> bool {
        let h1 = hash;
        let h2 = hash.rotate_left(21);
        let h3 = hash.rotate_left(42);

        self.get_bit(h1 % self.num_bits as u64)
            && self.get_bit(h2 % self.num_bits as u64)
            && self.get_bit(h3 % self.num_bits as u64)
    }

    fn set_bit(&mut self, bit: u64) {
        let word = (bit / 64) as usize;
        let bit_in_word = bit % 64;
        self.bits[word] |= 1 << bit_in_word;
    }

    fn get_bit(&self, bit: u64) -> bool {
        let word = (bit / 64) as usize;
        let bit_in_word = bit % 64;
        (self.bits[word] >> bit_in_word) & 1 == 1
    }
}

/// Track filter mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackFilterMode {
    /// Allow only tracks in the allowlist.
    Allowlist,
    /// Allow all tracks except those in the denylist.
    Denylist,
}

/// Track Filter for controlling track selection at subscription time.
///
/// This is the second stage of the filter pipeline, applied when a
/// SUBSCRIBE message is received.
///
/// # Performance
/// - Bloom filter for fast rejection: O(1)
/// - Exact set lookup on Bloom hit: O(1) average
/// - Memory: ~1KB for Bloom filter + tracks set
pub struct TrackFilter {
    /// Filter mode (allowlist or denylist).
    mode: TrackFilterMode,

    /// Bloom filter for fast negative lookups.
    bloom: BloomFilter,

    /// Exact track set for positive confirmation.
    tracks: FxHashSet<u64>, // Store only hashes for memory efficiency

    /// Full track identifiers (for debugging/inspection).
    track_ids: Vec<TrackIdentifier>,

    /// Track selection limits.
    limits: TrackLimits,

    /// Current count of selected tracks.
    selected_count: AtomicU64,

    /// Current count of deselected tracks.
    deselected_count: AtomicU64,

    /// Whether this filter is enabled.
    enabled: AtomicBool,

    /// Statistics collector.
    pub stats: FilterStats,
}

impl TrackFilter {
    /// Creates a new allowlist track filter.
    pub fn allowlist() -> Self {
        Self::new(TrackFilterMode::Allowlist)
    }

    /// Creates a new denylist track filter.
    pub fn denylist() -> Self {
        Self::new(TrackFilterMode::Denylist)
    }

    /// Creates a new track filter with the specified mode.
    pub fn new(mode: TrackFilterMode) -> Self {
        Self {
            mode,
            bloom: BloomFilter::for_capacity(100),
            tracks: FxHashSet::default(),
            track_ids: Vec::new(),
            limits: TrackLimits::unlimited(),
            selected_count: AtomicU64::new(0),
            deselected_count: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
            stats: FilterStats::new(),
        }
    }

    /// Creates a filter with statistics enabled.
    pub fn with_stats(mode: TrackFilterMode) -> Self {
        let mut filter = Self::new(mode);
        filter.stats = FilterStats::enabled();
        filter
    }

    /// Sets the track limits.
    pub fn set_limits(&mut self, limits: TrackLimits) {
        self.limits = limits;
    }

    /// Enables this filter stage.
    #[inline]
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Disables this filter stage.
    #[inline]
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Relaxed);
    }

    /// Returns true if this filter is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Adds a track to the filter.
    pub fn add_track(&mut self, track: TrackIdentifier) {
        let hash = track.hash();

        // Resize bloom filter if needed
        if self.tracks.len() >= self.bloom.num_bits / 10 {
            self.rebuild_bloom(self.tracks.len() * 2);
        }

        self.bloom.insert(hash);
        self.tracks.insert(hash);
        self.track_ids.push(track);
    }

    /// Adds a track by namespace and name.
    pub fn add(&mut self, namespace: &TrackNamespace, name: &str) {
        let track = TrackIdentifier::new(namespace, name);
        self.add_track(track);
    }

    /// Rebuilds the bloom filter with a new capacity.
    fn rebuild_bloom(&mut self, new_capacity: usize) {
        self.bloom = BloomFilter::for_capacity(new_capacity);
        for &hash in &self.tracks {
            self.bloom.insert(hash);
        }
    }

    /// Checks if a track is in the filter's set.
    fn is_in_set(&self, namespace: &TrackNamespace, name: &str) -> bool {
        let hash = TrackIdentifier::compute_hash(namespace, name);

        // Fast path: Bloom filter says definitely not present
        if !self.bloom.may_contain(hash) {
            return false;
        }

        // Slow path: Check exact set
        self.tracks.contains(&hash)
    }

    /// Checks if a track should be accepted based on the filter rules.
    ///
    /// Returns true if the track should be accepted.
    pub fn should_accept(&self, namespace: &TrackNamespace, name: &str) -> bool {
        if !self.is_enabled() {
            return true;
        }

        // Check limits first
        if self.limits.max_selected > 0 {
            let current = self.selected_count.load(Ordering::Relaxed);
            if current >= self.limits.max_selected {
                return false;
            }
        }

        // Check against track set based on mode
        let in_set = self.is_in_set(namespace, name);

        match self.mode {
            TrackFilterMode::Allowlist => in_set,
            TrackFilterMode::Denylist => !in_set,
        }
    }

    /// Filters a track and records statistics.
    /// Also updates selected/deselected counts.
    pub fn filter(&self, namespace: &TrackNamespace, name: &str) -> bool {
        let result = self.should_accept(namespace, name);

        if result {
            self.selected_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.deselected_count.fetch_add(1, Ordering::Relaxed);
        }

        self.stats.record(result);
        result
    }

    /// Returns current selected count.
    #[inline]
    pub fn selected_count(&self) -> u64 {
        self.selected_count.load(Ordering::Relaxed)
    }

    /// Returns current deselected count.
    #[inline]
    pub fn deselected_count(&self) -> u64 {
        self.deselected_count.load(Ordering::Relaxed)
    }

    /// Returns the number of tracks in the filter.
    #[inline]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Estimates memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        let base = std::mem::size_of::<Self>();
        let bloom = self.bloom.bits.len() * 8;
        let tracks = self.tracks.len() * std::mem::size_of::<u64>();
        let track_ids: usize = self
            .track_ids
            .iter()
            .map(|t| std::mem::size_of::<TrackIdentifier>() + t.name.len())
            .sum();
        base + bloom + tracks + track_ids
    }
}

impl Default for TrackFilter {
    fn default() -> Self {
        Self::allowlist()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ns(path: &str) -> TrackNamespace {
        TrackNamespace::from_utf8_path(path)
    }

    #[test]
    fn test_allowlist_empty() {
        let filter = TrackFilter::allowlist();
        // Empty allowlist should reject everything
        assert!(!filter.should_accept(&ns("live/game"), "video"));
    }

    #[test]
    fn test_allowlist_match() {
        let mut filter = TrackFilter::allowlist();
        filter.add(&ns("live/game"), "video");

        assert!(filter.should_accept(&ns("live/game"), "video"));
        assert!(!filter.should_accept(&ns("live/game"), "audio"));
        assert!(!filter.should_accept(&ns("live/other"), "video"));
    }

    #[test]
    fn test_denylist_empty() {
        let filter = TrackFilter::denylist();
        // Empty denylist should accept everything
        assert!(filter.should_accept(&ns("live/game"), "video"));
    }

    #[test]
    fn test_denylist_match() {
        let mut filter = TrackFilter::denylist();
        filter.add(&ns("live/game"), "video");

        assert!(!filter.should_accept(&ns("live/game"), "video"));
        assert!(filter.should_accept(&ns("live/game"), "audio"));
        assert!(filter.should_accept(&ns("live/other"), "video"));
    }

    #[test]
    fn test_limits() {
        let mut filter = TrackFilter::denylist();
        filter.set_limits(TrackLimits::max_tracks(2));

        assert!(filter.filter(&ns("live/game"), "video"));
        assert!(filter.filter(&ns("live/game"), "audio"));
        assert!(!filter.filter(&ns("live/game"), "chat")); // Exceeds limit
    }

    #[test]
    fn test_disabled() {
        let mut filter = TrackFilter::allowlist();
        filter.add(&ns("live/game"), "video");
        filter.disable();

        // Should accept everything when disabled
        assert!(filter.should_accept(&ns("live/other"), "audio"));
    }

    #[test]
    fn test_stats() {
        let mut filter = TrackFilter::with_stats(TrackFilterMode::Allowlist);
        filter.add(&ns("live/game"), "video");

        filter.filter(&ns("live/game"), "video"); // Pass
        filter.filter(&ns("live/game"), "audio"); // Fail

        let stats = filter.stats.snapshot();
        assert_eq!(stats.passed, 1);
        assert_eq!(stats.filtered, 1);
    }

    #[test]
    fn test_bloom_rebuild() {
        let mut filter = TrackFilter::allowlist();

        // Add many tracks to trigger bloom filter rebuild
        for i in 0..200 {
            filter.add(&ns(&format!("live/game{}", i)), "video");
        }

        // Should still find tracks after rebuild
        assert!(filter.should_accept(&ns("live/game0"), "video"));
        assert!(filter.should_accept(&ns("live/game199"), "video"));
        assert!(!filter.should_accept(&ns("live/game200"), "video"));
    }
}
