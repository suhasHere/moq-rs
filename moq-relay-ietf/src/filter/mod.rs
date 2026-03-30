//! High-performance filtering pipeline for MoQ relay.
//!
//! This module implements a multi-stage filtering pipeline:
//! 1. Track Extension Filter - filters based on track extension metadata
//! 2. Track Filter - filters based on track namespace/name with limits
//! 3. Object Filter - filters individual objects based on various criteria
//! 4. Top-N Filter - selects top N tracks based on metrics in extension headers
//!
//! The pipeline is designed for extreme performance at large-scale fanouts (100k+ subscribers).

mod config;
mod object;
mod pipeline;
mod range;
mod stats;
mod topn;
mod track;
mod track_ext;

pub use config::{FilterArgs, FilterConfig};
pub use object::{
    GroupFilter, LocationFilter, ObjectFilter, ObjectFilterParams, ObjectInfo, PriorityBitmap,
};
pub use pipeline::{FilterPipeline, PipelineReport};
pub use range::RangeSet;
pub use stats::{FilterStats, FilterStatsSnapshot};
pub use topn::{TopNConfig, TopNFilter, TopNStats, TrackId, DEFAULT_METRIC_EXTENSION_TYPE};
pub use track::{TrackFilter, TrackFilterMode, TrackIdentifier, TrackLimits};
pub use track_ext::{ExtensionMatcher, TrackExtensionFilter};
