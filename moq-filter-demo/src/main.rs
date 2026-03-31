//! MoQ Filter Demo - Demonstrates Subscribe Namespace with Top-N Track Selection
//!
//! This example shows how to use TRACK_FILTER (PR #1518) for top-N track selection
//! based on property values (e.g., active speaker selection).
//!
//! # Namespace Structure
//!
//! - Conference namespace: `conference/<room-id>`
//! - Participant tracks: `conference/<room-id>/<participant-id>/audio`
//!
//! # Usage
//!
//! ```bash
//! # Publisher mode - simulate a participant with audio activity
//! moq-filter-demo --server https://localhost:4443 --room meeting1 --publish --name Alice
//!
//! # Subscriber mode - subscribe to top N active speakers
//! moq-filter-demo --server https://localhost:4443 --room meeting1 --top-n 3
//! ```

use moq_native_ietf::quic;

use anyhow::Context;
use bytes::Bytes;
use clap::Parser;
use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use rand::Rng;
use std::time::Duration;

use moq_transport::{
    coding::TrackNamespace,
    message::{SubscribeNamespace, TrackFilter, TrackSelector},
    serve::{self, TracksReader},
    session::{Publisher, SessionError},
};

#[derive(Parser, Clone)]
#[command(name = "moq-filter-demo")]
#[command(about = "Demo of MoQ Subscribe Namespace filtering with top-N track selection")]
struct Cli {
    /// Listen for UDP packets on the given address.
    #[arg(long, default_value = "[::]:0")]
    bind: std::net::SocketAddr,

    /// Connect to the given URL starting with https://
    #[arg(short, long, default_value = "https://localhost:4443")]
    server: url::Url,

    /// The TLS configuration.
    #[command(flatten)]
    tls: moq_native_ietf::tls::Args,

    /// Conference room identifier.
    #[arg(short, long)]
    room: String,

    /// Publish mode - act as a conference participant.
    #[arg(short, long)]
    publish: bool,

    /// Participant name (for publish mode).
    #[arg(short, long, default_value = "participant")]
    name: String,

    /// Subscribe with top-N filter - select N most active speakers.
    #[arg(short = 'n', long, default_value = "3")]
    top_n: u64,

    /// Property type for filtering (audio activity level).
    #[arg(long, default_value = "256")]
    property_type: u64,

    /// Timeout in milliseconds before track deselection.
    #[arg(long, default_value = "5000")]
    timeout_ms: u64,

    /// Simulated audio activity update interval in milliseconds.
    #[arg(long, default_value = "1000")]
    activity_interval_ms: u64,
}

/// Serve subscriptions to published tracks.
async fn serve_subscriptions(
    mut publisher: Publisher,
    tracks: TracksReader,
) -> Result<(), SessionError> {
    let mut tasks: FuturesUnordered<futures::future::BoxFuture<'static, ()>> =
        FuturesUnordered::new();

    loop {
        tokio::select! {
            Some(subscribed) = publisher.subscribed() => {
                let info = subscribed.info.clone();
                let tracks = tracks.clone();
                log::info!("serving subscribe: {:?}", info);

                tasks.push(async move {
                    if let Err(err) = Publisher::serve_subscribe(subscribed, tracks).await {
                        log::warn!("failed serving subscribe: {:?}, error: {}", info, err);
                    }
                }.boxed());
            }
            _ = tasks.next(), if !tasks.is_empty() => {}
            else => return Ok(()),
        }
    }
}

/// Simulates audio activity by publishing periodic updates with varying levels.
async fn publish_audio_activity(
    mut tracks_writer: serve::TracksWriter,
    participant_name: &str,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let track_name = "audio";
    let track_writer = tracks_writer
        .create(track_name)
        .context("failed to create track")?;
    let mut subgroups = track_writer.subgroups()?;

    let mut rng = rand::thread_rng();
    let mut sequence = 0u64;

    println!("Publishing audio activity as '{}'", participant_name);
    println!("Activity level will vary randomly between 0-255");
    println!();

    loop {
        // Simulate varying audio activity level (0-255)
        let activity_level: u8 = rng.gen();

        // Create message with activity level in metadata
        let message = format!(
            "{{\"participant\":\"{}\",\"activity\":{},\"seq\":{}}}",
            participant_name, activity_level, sequence
        );

        let mut subgroup = subgroups.append(activity_level)?; // Priority = activity level
        subgroup.write(Bytes::from(message.clone()))?;

        println!(
            "[{}] Activity: {:3} | Seq: {}",
            participant_name, activity_level, sequence
        );

        sequence += 1;
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }
}

/// Demonstrates track selection algorithm locally (without network).
fn demo_track_selector(top_n: u64, property_type: u64, timeout_ms: u64) {
    println!("==============================================");
    println!("  Track Selector Demo (Top-{} Selection)", top_n);
    println!("==============================================");
    println!();
    println!("Property Type: 0x{:X} ({})", property_type, property_type);
    println!("Timeout: {} ms", timeout_ms);
    println!();

    // Create filter
    let filter = TrackFilter::new(property_type, top_n, timeout_ms)
        .expect("Invalid filter parameters");

    println!("Track Filter Configuration:");
    println!("  - property_type: 0x{:X}", filter.property_type);
    println!("  - max_tracks_selected: {}", filter.max_tracks_selected);
    println!("  - timeout_ms: {}", filter.timeout_ms);
    println!();

    // Create selector
    let mut selector = TrackSelector::new(filter);

    // Simulate 5 participants with different activity levels
    let participants = [
        (1, "Alice", 80u64),
        (2, "Bob", 150u64),
        (3, "Charlie", 200u64),
        (4, "Diana", 50u64),
        (5, "Eve", 180u64),
    ];

    println!("Initial participant activity levels:");
    println!("----------------------------------------------");
    for (id, name, activity) in &participants {
        println!("  {} (ID {}): activity = {}", name, id, activity);
    }
    println!();

    // Add participants to selector
    println!("Adding participants to selector...");
    println!("----------------------------------------------");
    let mut current_time = 0u64;

    for (id, _name, activity) in &participants {
        let changes = selector.update_track(*id, *activity, current_time);

        for (track_id, old_state, new_state) in &changes {
            let participant_name = participants
                .iter()
                .find(|(i, _, _)| i == track_id)
                .map(|(_, n, _)| *n)
                .unwrap_or("Unknown");

            println!(
                "  {} (ID {}): {:?} -> {:?}",
                participant_name, track_id, old_state, new_state
            );
        }
    }
    println!();

    // Show current selection
    println!("Current selection (Top-{}):", top_n);
    println!("----------------------------------------------");
    let selected = selector.selected_tracks();
    for track_id in &selected {
        let participant_name = participants
            .iter()
            .find(|(i, _, _)| *i == *track_id)
            .map(|(_, n, _)| *n)
            .unwrap_or("Unknown");

        println!("  [SELECTED] {} (ID {})", participant_name, track_id);
    }

    for (id, name, _) in &participants {
        if !selected.contains(id) {
            println!("  [DESELECTED] {} (ID {})", name, id);
        }
    }
    println!();

    // Simulate activity change - Alice becomes very active
    println!("Simulating activity change: Alice becomes very active (250)");
    println!("----------------------------------------------");
    current_time += 1000;
    let changes = selector.update_track(1, 250, current_time);

    for (track_id, old_state, new_state) in &changes {
        let participant_name = participants
            .iter()
            .find(|(i, _, _)| i == track_id)
            .map(|(_, n, _)| *n)
            .unwrap_or("Unknown");

        println!(
            "  {} (ID {}): {:?} -> {:?}",
            participant_name, track_id, old_state, new_state
        );
    }
    println!();

    // Show updated selection
    println!("Updated selection (Top-{}):", top_n);
    println!("----------------------------------------------");
    let selected = selector.selected_tracks();
    for track_id in &selected {
        let participant_name = participants
            .iter()
            .find(|(i, _, _)| *i == *track_id)
            .map(|(_, n, _)| *n)
            .unwrap_or("Unknown");

        let state = selector.get_state(*track_id);
        println!("  [SELECTED] {} (ID {}) - {:?}", participant_name, track_id, state);
    }

    for (id, name, _) in &participants {
        if !selected.contains(id) {
            let state = selector.get_state(*id);
            println!("  [DESELECTED] {} (ID {}) - {:?}", name, id, state);
        }
    }
    println!();

    // Show how to create SubscribeNamespace with filter
    println!("Creating SubscribeNamespace with TrackFilter:");
    println!("----------------------------------------------");
    let mut msg = SubscribeNamespace::new(
        42,
        TrackNamespace::from_utf8_path("conference/meeting1"),
        1,
    );

    let track_filter = TrackFilter::new(property_type, top_n, timeout_ms).unwrap();
    msg.set_track_filter(track_filter);

    println!("  SubscribeNamespace {{");
    println!("    id: {},", msg.id);
    println!("    namespace_prefix: {:?},", msg.track_namespace_prefix);
    println!("    forward: {},", msg.forward);
    println!("    track_filter: {:?}", msg.track_filter());
    println!("  }}");
    println!();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Disable tracing spam
    let tracer = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(tracer).unwrap();

    let config = Cli::parse();

    // If not in publish mode and not connecting to server, run local demo
    if !config.publish {
        // Run the local track selector demo
        demo_track_selector(config.top_n, config.property_type, config.timeout_ms);
        return Ok(());
    }

    // Network mode - publish or subscribe
    let tls = config.tls.load()?;

    // Create QUIC endpoint
    let quic = quic::Endpoint::new(quic::Config::new(config.bind, None, tls))?;

    log::info!("connecting to server: url={}", config.server);

    // Connect to server
    let (session, connection_id) = quic.client.connect(&config.server, None).await?;

    log::info!(
        "connected with CID: {} (use this to look up qlog/mlog on server)",
        connection_id
    );

    if config.publish {
        // Publisher mode - act as a conference participant
        let (session, mut publisher) = Publisher::connect(session)
            .await
            .context("failed to create MoQ Transport session")?;

        // Create namespace: conference/<room>/<participant>
        let namespace_path = format!("conference/{}/{}", config.room, config.name);
        let namespace = TrackNamespace::from_utf8_path(&namespace_path);

        let (tracks_writer, _, tracks_reader) = serve::Tracks {
            namespace: namespace.clone(),
        }
        .produce();

        let publish_ns = publisher
            .publish_namespace(namespace)
            .await
            .context("failed to register namespace")?;

        println!("==============================================");
        println!("  Publishing as: {}", config.name);
        println!("  Room: {}", config.room);
        println!("  Namespace: conference/{}/{}", config.room, config.name);
        println!("==============================================");
        println!();

        tokio::select! {
            res = session.run() => res.context("session error")?,
            res = publish_audio_activity(tracks_writer, &config.name, config.activity_interval_ms) => res?,
            res = serve_subscriptions(publisher, tracks_reader) => res.context("failed to serve tracks")?,
            res = publish_ns.closed() => res.context("namespace closed")?,
        }
    }

    Ok(())
}
