//! Lock-free statistics collection for filter performance monitoring.
//!
//! Uses atomic operations for zero-contention updates in the hot path.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Lock-free statistics collector for a single filter stage.
///
/// All operations use relaxed ordering for maximum performance,
/// as we only need eventual consistency for statistics.
#[derive(Default)]
pub struct FilterStats {
    /// Number of items that passed the filter.
    passed: AtomicU64,

    /// Number of items filtered out.
    filtered: AtomicU64,

    /// Total processing time in nanoseconds.
    total_ns: AtomicU64,

    /// Number of timing samples (for computing average).
    timing_samples: AtomicU64,

    /// Peak memory usage estimate in bytes.
    peak_memory_bytes: AtomicU64,

    /// Whether stats collection is enabled.
    enabled: AtomicU64, // Using u64 for alignment, 0 = disabled, 1 = enabled
}

impl FilterStats {
    /// Creates a new stats collector.
    #[inline]
    pub const fn new() -> Self {
        Self {
            passed: AtomicU64::new(0),
            filtered: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            timing_samples: AtomicU64::new(0),
            peak_memory_bytes: AtomicU64::new(0),
            enabled: AtomicU64::new(0),
        }
    }

    /// Creates a new stats collector with collection enabled.
    #[inline]
    pub fn enabled() -> Self {
        Self {
            passed: AtomicU64::new(0),
            filtered: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            timing_samples: AtomicU64::new(0),
            peak_memory_bytes: AtomicU64::new(0),
            enabled: AtomicU64::new(1),
        }
    }

    /// Enables stats collection.
    #[inline]
    pub fn enable(&self) {
        self.enabled.store(1, Ordering::Relaxed);
    }

    /// Disables stats collection.
    #[inline]
    pub fn disable(&self) {
        self.enabled.store(0, Ordering::Relaxed);
    }

    /// Returns true if stats collection is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) != 0
    }

    /// Records an item that passed the filter.
    #[inline(always)]
    pub fn record_passed(&self) {
        if self.is_enabled() {
            self.passed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records an item that was filtered out.
    #[inline(always)]
    pub fn record_filtered(&self) {
        if self.is_enabled() {
            self.filtered.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records the result of a filter operation.
    #[inline(always)]
    pub fn record(&self, passed: bool) {
        if passed {
            self.record_passed();
        } else {
            self.record_filtered();
        }
    }

    /// Records timing from a start instant.
    #[inline(always)]
    pub fn record_timing(&self, start: Instant) {
        if self.is_enabled() {
            let elapsed_ns = start.elapsed().as_nanos() as u64;
            self.total_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
            self.timing_samples.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Records timing in nanoseconds directly.
    #[inline(always)]
    pub fn record_timing_ns(&self, ns: u64) {
        if self.is_enabled() {
            self.total_ns.fetch_add(ns, Ordering::Relaxed);
            self.timing_samples.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Updates peak memory usage if the new value is higher.
    #[inline]
    pub fn update_memory(&self, bytes: u64) {
        if self.is_enabled() {
            let mut current = self.peak_memory_bytes.load(Ordering::Relaxed);
            while bytes > current {
                match self.peak_memory_bytes.compare_exchange_weak(
                    current,
                    bytes,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(c) => current = c,
                }
            }
        }
    }

    /// Takes a snapshot of the current statistics.
    pub fn snapshot(&self) -> FilterStatsSnapshot {
        FilterStatsSnapshot {
            passed: self.passed.load(Ordering::Relaxed),
            filtered: self.filtered.load(Ordering::Relaxed),
            total_ns: self.total_ns.load(Ordering::Relaxed),
            timing_samples: self.timing_samples.load(Ordering::Relaxed),
            peak_memory_bytes: self.peak_memory_bytes.load(Ordering::Relaxed),
        }
    }

    /// Resets all statistics to zero.
    pub fn reset(&self) {
        self.passed.store(0, Ordering::Relaxed);
        self.filtered.store(0, Ordering::Relaxed);
        self.total_ns.store(0, Ordering::Relaxed);
        self.timing_samples.store(0, Ordering::Relaxed);
        self.peak_memory_bytes.store(0, Ordering::Relaxed);
    }
}

/// A point-in-time snapshot of filter statistics.
#[derive(Clone, Debug, Default)]
pub struct FilterStatsSnapshot {
    /// Number of items that passed the filter.
    pub passed: u64,

    /// Number of items filtered out.
    pub filtered: u64,

    /// Total processing time in nanoseconds.
    pub total_ns: u64,

    /// Number of timing samples.
    pub timing_samples: u64,

    /// Peak memory usage estimate in bytes.
    pub peak_memory_bytes: u64,
}

impl FilterStatsSnapshot {
    /// Returns the total number of items processed.
    #[inline]
    pub fn total(&self) -> u64 {
        self.passed + self.filtered
    }

    /// Returns the pass rate as a value between 0.0 and 1.0.
    #[inline]
    pub fn pass_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            1.0
        } else {
            self.passed as f64 / total as f64
        }
    }

    /// Returns the filter rate as a value between 0.0 and 1.0.
    #[inline]
    pub fn filter_rate(&self) -> f64 {
        1.0 - self.pass_rate()
    }

    /// Returns the average processing time per item in nanoseconds.
    #[inline]
    pub fn avg_ns(&self) -> f64 {
        if self.timing_samples == 0 {
            0.0
        } else {
            self.total_ns as f64 / self.timing_samples as f64
        }
    }

    /// Returns the average processing time per item in microseconds.
    #[inline]
    pub fn avg_us(&self) -> f64 {
        self.avg_ns() / 1000.0
    }

    /// Returns peak memory usage in kilobytes.
    #[inline]
    pub fn peak_memory_kb(&self) -> f64 {
        self.peak_memory_bytes as f64 / 1024.0
    }

    /// Returns peak memory usage in megabytes.
    #[inline]
    pub fn peak_memory_mb(&self) -> f64 {
        self.peak_memory_bytes as f64 / (1024.0 * 1024.0)
    }

    /// Merges another snapshot into this one (for aggregation).
    pub fn merge(&mut self, other: &FilterStatsSnapshot) {
        self.passed += other.passed;
        self.filtered += other.filtered;
        self.total_ns += other.total_ns;
        self.timing_samples += other.timing_samples;
        self.peak_memory_bytes = self.peak_memory_bytes.max(other.peak_memory_bytes);
    }
}

impl fmt::Display for FilterStatsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  Passed:      {:>12}", self.passed)?;
        writeln!(f, "  Filtered:    {:>12}", self.filtered)?;
        writeln!(f, "  Total:       {:>12}", self.total())?;
        writeln!(f, "  Pass Rate:   {:>11.2}%", self.pass_rate() * 100.0)?;
        writeln!(f, "  Avg Time:    {:>10.2}ns", self.avg_ns())?;
        write!(f, "  Peak Memory: {:>10.2}KB", self.peak_memory_kb())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_counting() {
        let stats = FilterStats::enabled();

        stats.record_passed();
        stats.record_passed();
        stats.record_filtered();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.passed, 2);
        assert_eq!(snapshot.filtered, 1);
        assert_eq!(snapshot.total(), 3);
    }

    #[test]
    fn test_pass_rate() {
        let stats = FilterStats::enabled();

        for _ in 0..80 {
            stats.record_passed();
        }
        for _ in 0..20 {
            stats.record_filtered();
        }

        let snapshot = stats.snapshot();
        assert!((snapshot.pass_rate() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_disabled_stats() {
        let stats = FilterStats::new(); // Disabled by default

        stats.record_passed();
        stats.record_filtered();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.passed, 0);
        assert_eq!(snapshot.filtered, 0);
    }

    #[test]
    fn test_reset() {
        let stats = FilterStats::enabled();

        stats.record_passed();
        stats.record_filtered();

        stats.reset();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.total(), 0);
    }

    #[test]
    fn test_memory_tracking() {
        let stats = FilterStats::enabled();

        stats.update_memory(1000);
        stats.update_memory(500); // Should not update (lower)
        stats.update_memory(2000);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.peak_memory_bytes, 2000);
    }
}
