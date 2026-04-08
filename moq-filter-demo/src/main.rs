//! MoQ Filter Demo - Demonstrates Subscribe Namespace with Top-N Track Selection
//!
//! This example shows how to use TRACK_FILTER for top-N track selection
//! with self-exclusion support.
//!
//! # Modes
//!
//! - **Publish mode**: Act as a conference participant sending audio activity
//! - **Subscribe mode**: Subscribe to top-N active speakers
//! - **Both mode**: Publish AND subscribe on same connection (tests self-exclusion)
//!
//! # Usage
//!
//! ```bash
//! # Start relay with top-N enabled
//! moq-relay-ietf --topn-enabled --topn-count 3 --topn-metric-type 256 ...
//!
//! # Publisher mode - simulate a participant with audio activity
//! moq-filter-demo --server https://localhost:4443 --room meeting1 --publish --name Alice
//!
//! # Subscriber mode - subscribe to top N active speakers
//! moq-filter-demo --server https://localhost:4443 --room meeting1 --subscribe --top-n 3
//!
//! # Both mode - test self-exclusion (publish + subscribe on same connection)
//! moq-filter-demo --server https://localhost:4443 --room meeting1 --both --name Alice --top-n 3
//! ```
//!
//! In "both" mode, Alice should NOT see her own track in the top-N results (self-exclusion).

use moq_native_ietf::quic;

use anyhow::Context;
use bytes::Bytes;
use clap::Parser;
use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use rand::Rng;
use std::time::Duration;

use moq_transport::{
    coding::TrackNamespace,
    message::{TrackFilter, TrackSelector},
    serve::{self, TracksReader},
    session::{Publisher, SessionError, Subscriber},
};

#[derive(Parser, Clone)]
#[command(name = "moq-filter-demo")]
#[command(about = "Demo of MoQ Subscribe Namespace filtering with top-N track selection and self-exclusion")]
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

    /// Conference room identifier (required for network modes).
    #[arg(short, long, default_value = "default")]
    room: String,

    /// Publish mode - act as a conference participant.
    #[arg(short, long)]
    publish: bool,

    /// Subscribe mode - subscribe to top-N tracks.
    #[arg(long)]
    subscribe: bool,

    /// Both mode - publish AND subscribe (tests self-exclusion).
    #[arg(long)]
    both: bool,

    /// Participant name (for publish mode).
    #[arg(long, default_value = "participant")]
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

    /// Run local demo without network connection.
    #[arg(long)]
    local_demo: bool,
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
    property_type: u64,
) -> anyhow::Result<()> {
    let track_name = "audio";
    let track_writer = tracks_writer
        .create(track_name)
        .context("failed to create track")?;
    let mut subgroups = track_writer.subgroups()?;

    let mut rng = rand::thread_rng();
    let mut sequence = 0u64;

    println!("[PUB] Publishing audio activity as '{}'", participant_name);
    println!("[PUB] Metric extension type: 0x{:X}", property_type);
    println!();

    loop {
        // Simulate varying audio activity level (0-255)
        let activity_level: u8 = rng.gen();

        // Create message with activity level in metadata
        let message = format!(
            "{{\"participant\":\"{}\",\"activity\":{},\"seq\":{}}}",
            participant_name, activity_level, sequence
        );

        // The priority field carries the activity level for now
        // In a real implementation, extension headers would carry the metric
        let mut subgroup = subgroups.append(activity_level)?;
        subgroup.write(Bytes::from(message.clone()))?;

        println!(
            "[PUB] {} | Activity: {:3} | Seq: {}",
            participant_name, activity_level, sequence
        );

        sequence += 1;
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }
}

/// Subscribe to a namespace with top-N filtering.
async fn subscribe_to_namespace(
    mut subscriber: Subscriber,
    room: &str,
    top_n: u64,
    property_type: u64,
    timeout_ms: u64,
    self_name: Option<&str>,
) -> anyhow::Result<()> {
    // Create namespace prefix: conference/<room>
    let namespace_prefix = TrackNamespace::from_utf8_path(&format!("conference/{}", room));

    // Create track filter for top-N selection
    let track_filter = TrackFilter::new(property_type, top_n, timeout_ms)
        .map_err(|e| anyhow::anyhow!("Invalid filter: {}", e))?;

    println!("[SUB] Subscribing to namespace: conference/{}", room);
    println!("[SUB] Track filter: top-{}, property_type=0x{:X}, timeout={}ms",
             top_n, property_type, timeout_ms);
    if let Some(name) = self_name {
        println!("[SUB] Self-exclusion active: {} should NOT appear in results", name);
    }
    println!();

    // Send SUBSCRIBE_NAMESPACE with filter
    let subscribe_ns = subscriber
        .subscribe_ns_with_filter(namespace_prefix.clone(), Some(track_filter))
        .context("failed to send SUBSCRIBE_NAMESPACE")?;

    println!("[SUB] SUBSCRIBE_NAMESPACE sent, waiting for PUBLISH notifications...");
    println!();

    // Track received publishes
    let mut received_tracks: Vec<String> = Vec::new();

    // Handle incoming messages
    loop {
        tokio::select! {
            // Check for PUBLISH notifications (via the subscription)
            result = subscribe_ns.closed() => {
                match result {
                    Ok(()) => {
                        println!("[SUB] Subscription closed normally");
                        break;
                    }
                    Err(e) => {
                        println!("[SUB] Subscription error: {}", e);
                        break;
                    }
                }
            }

            // Also listen for any publish messages
            publish = subscriber.publish_received() => {
                if let Some(publish) = publish {
                    let ns = publish.info.track_namespace.to_string();
                    let track = &publish.info.track_name;
                    let full_name = format!("{}/{}", ns, track);

                    // Check if this is our own track (for self-exclusion verification)
                    let is_self = self_name.map(|n| ns.contains(n)).unwrap_or(false);

                    if is_self {
                        println!("[SUB] *** SELF-EXCLUSION FAILED *** Received own track: {}", full_name);
                    } else {
                        println!("[SUB] Received PUBLISH: {}", full_name);
                        if !received_tracks.contains(&full_name) {
                            received_tracks.push(full_name.clone());
                        }
                    }

                    // Print current top-N
                    println!("[SUB] Current tracks received ({}):", received_tracks.len());
                    for (i, t) in received_tracks.iter().enumerate() {
                        println!("[SUB]   {}. {}", i + 1, t);
                    }
                    println!();
                } else {
                    // No more publishes
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    Ok(())
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
        (1, "Alice", 200u64),
        (2, "Bob", 150u64),
        (3, "Charlie", 100u64),
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
    let current_time = 0u64;

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

    // Demonstrate self-exclusion concept
    println!("Self-Exclusion Demo:");
    println!("----------------------------------------------");
    println!("If Alice (rank 1) is also a subscriber:");
    println!("  - Global top-3: Alice, Eve, Bob");
    println!("  - Alice's view:  Eve, Bob, Charlie (excludes self)");
    println!();
    println!("Waterline calculation:");
    println!("  - Alice's track is in top-3, so her waterline = 4th highest = 100 (Charlie)");
    println!("  - Diana's waterline = global threshold = 150 (Bob, 3rd place)");
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

    // Local demo mode
    if config.local_demo {
        demo_track_selector(config.top_n, config.property_type, config.timeout_ms);
        return Ok(());
    }

    // Validate mode selection
    let mode_count = [config.publish, config.subscribe, config.both]
        .iter()
        .filter(|&&x| x)
        .count();

    if mode_count == 0 {
        println!("No mode selected. Use --publish, --subscribe, or --both");
        println!("Or use --local-demo for offline demonstration");
        return Ok(());
    }

    if mode_count > 1 && !config.both {
        anyhow::bail!("Please select only one mode: --publish, --subscribe, or --both");
    }

    // Network mode
    let tls = config.tls.load()?;
    let quic = quic::Endpoint::new(quic::Config::new(config.bind, None, tls))?;

    log::info!("connecting to server: url={}", config.server);

    let (session, connection_id) = quic.client.connect(&config.server, None).await?;

    log::info!(
        "connected with CID: {} (use this to look up qlog/mlog on server)",
        connection_id
    );

    println!("==============================================");
    println!("  MoQ Filter Demo - Self-Exclusion Test");
    println!("==============================================");
    println!("Server: {}", config.server);
    println!("Room: {}", config.room);
    println!("Connection ID: {}", connection_id);
    println!();

    if config.both {
        // BOTH mode: Publish AND Subscribe on same connection
        // This tests self-exclusion - we should NOT see our own track
        println!("Mode: BOTH (publish + subscribe)");
        println!("Testing self-exclusion for: {}", config.name);
        println!();

        // Create session with both publisher and subscriber
        // Session::connect returns (Session, Publisher, Subscriber) directly
        let (session, publisher, subscriber) = moq_transport::session::Session::connect(session, None)
            .await
            .context("failed to create MoQ session")?;

        // Create namespace: conference/<room>/<participant>
        let namespace_path = format!("conference/{}/{}", config.room, config.name);
        let namespace = TrackNamespace::from_utf8_path(&namespace_path);

        let (tracks_writer, _, tracks_reader) = serve::Tracks {
            namespace: namespace.clone(),
        }
        .produce();

        let mut pub_clone = publisher.clone();
        let publish_ns = pub_clone
            .publish_namespace(namespace)
            .await
            .context("failed to register namespace")?;

        println!("[BOTH] Publishing as: {}", config.name);
        println!("[BOTH] Subscribing to room with top-{} filter", config.top_n);
        println!("[BOTH] If self-exclusion works, '{}' should NOT appear in received tracks", config.name);
        println!();

        let name = config.name.clone();
        let room = config.room.clone();
        let top_n = config.top_n;
        let property_type = config.property_type;
        let timeout_ms = config.timeout_ms;
        let interval_ms = config.activity_interval_ms;

        tokio::select! {
            res = session.run() => { let _: Result<(), SessionError> = res; }
            res = publish_audio_activity(tracks_writer, &name, interval_ms, property_type) => { res?; }
            res = serve_subscriptions(publisher, tracks_reader) => { res.context("failed to serve tracks")?; }
            res = subscribe_to_namespace(subscriber, &room, top_n, property_type, timeout_ms, Some(&name)) => { res?; }
            res = publish_ns.closed() => { res.context("namespace closed")?; }
        }
    } else if config.publish {
        // PUBLISH mode only
        println!("Mode: PUBLISH");
        println!();

        let (session, mut publisher) = Publisher::connect(session)
            .await
            .context("failed to create MoQ Transport session")?;

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

        println!("[PUB] Namespace: conference/{}/{}", config.room, config.name);
        println!();

        tokio::select! {
            res = session.run() => res.context("session error")?,
            res = publish_audio_activity(tracks_writer, &config.name, config.activity_interval_ms, config.property_type) => res?,
            res = serve_subscriptions(publisher, tracks_reader) => res.context("failed to serve tracks")?,
            res = publish_ns.closed() => res.context("namespace closed")?,
        }
    } else if config.subscribe {
        // SUBSCRIBE mode only
        println!("Mode: SUBSCRIBE");
        println!();

        let (session, subscriber) = Subscriber::connect(session)
            .await
            .context("failed to create MoQ Transport session")?;

        tokio::select! {
            res = session.run() => res.context("session error")?,
            res = subscribe_to_namespace(subscriber, &config.room, config.top_n, config.property_type, config.timeout_ms, None) => res?,
        }
    }

    Ok(())
}
