//! MoQ Relay library for building Media over QUIC relay servers.
//!
//! This crate provides the core relay functionality that can be embedded
//! into other applications. The relay handles:
//!
//! - Accepting QUIC connections from publishers and subscribers
//! - Routing media between local and remote endpoints
//! - Coordinating namespace/track registration across relay clusters
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use moq_relay_ietf::{Relay, RelayConfig, FileCoordinator};
//!
//! // Create a coordinator (FileCoordinator for multi-relay deployments)
//! let coordinator = FileCoordinator::new("/path/to/coordination/file", "https://relay.example.com");
//!
//! // Configure and create the relay
//! let relay = Relay::new(RelayConfig {
//!     bind: "[::]:443".parse().unwrap(),
//!     tls: tls_config,
//!     coordinator,
//!     // ... other options
//! })?;
//!
//! // Run the relay
//! relay.run().await?;
//! ```

mod api;
mod bandwidth;
mod catalog_dts;
mod consumer;
mod coordinator;
mod dts_service;
mod dts_tracker;
mod local;
mod producer;
mod quic_stats;
mod relay;
mod remote;
mod session;
mod subscriber_registry;
mod top_n_tracker;
mod web;

pub use api::*;
pub use bandwidth::*;
pub use catalog_dts::*;
pub use consumer::*;
pub use coordinator::*;
pub use dts_service::*;
pub use dts_tracker::*;
pub use local::*;
pub use producer::*;
pub use quic_stats::*;
pub use relay::*;
pub use remote::*;
pub use session::*;
pub use subscriber_registry::*;
pub use top_n_tracker::*;
pub use web::*;
