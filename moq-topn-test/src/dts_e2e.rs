//! DTS (Dynamic Track Switching) End-to-end tests.
//!
//! Tests ABR video streaming with multiple quality levels through a real relay.
//!
//! Two test modes:
//!
//! ## Mode 1: Simple SUBSCRIBE flow (--mode subscribe)
//! 1. Publisher connects and PUBLISH multiple quality tracks (1080p, 720p, 480p)
//! 2. Subscriber connects and sends SUBSCRIBEs with SWITCHING-SET-ASSIGNMENT params
//! 3. Relay forwards objects from selected track based on bandwidth estimation
//!
//! ## Mode 2: SUBSCRIBE_NAMESPACE + Top-N flow (--mode sub-ns-topn)
//! 1. Subscriber connects and SUBSCRIBE_NAMESPACE with TRACK_FILTER for top-N
//! 2. Multiple publishers connect and PUBLISH tracks under that namespace
//! 3. Publishers send objects with audio level extension headers
//! 4. Relay applies top-N filtering and forwards PUBLISH notifications
//! 5. DTS parameters control which quality level is forwarded for each track

use crate::Args;

use anyhow::{Context, Result};
use bytes::Bytes;
use moq_native_ietf::quic;
use moq_transport::{
    coding::{KeyValuePair, KeyValuePairs, TrackNamespace, Value},
    data::ExtensionHeaders,
    message::{DtsParams, PublishOk, SwitchingSetAssignment},
    serve::{Subgroup, Track, Tracks},
    session::Session,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info};
use url::Url;

/// Track quality definition for DTS
#[derive(Clone, Debug)]
struct QualityLevel {
    name: &'static str,
    throughput_kbps: u64,
    bitrate_bps: u64,
}

const QUALITY_LEVELS: &[QualityLevel] = &[
    QualityLevel { name: "1080p", throughput_kbps: 4000, bitrate_bps: 4_000_000 },
    QualityLevel { name: "720p", throughput_kbps: 2000, bitrate_bps: 2_000_000 },
    QualityLevel { name: "480p", throughput_kbps: 800, bitrate_bps: 800_000 },
];

// Property type for viewer count in top-N filtering
const PROPERTY_VIEWERS: u64 = 0x100;

/// Shared state for tracking test results
struct DtsTestState {
    received_from: HashMap<String, u64>,
    selection_changes: Vec<(Instant, String)>,
    publishes_received: Vec<String>,
}

impl DtsTestState {
    fn new() -> Self {
        Self {
            received_from: HashMap::new(),
            selection_changes: Vec::new(),
            publishes_received: Vec::new(),
        }
    }
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let relay_url: Url = args.relay.parse().context("invalid relay URL")?;

    // Determine test mode from args
    let mode = args.mode.as_deref().unwrap_or("subscribe");

    info!("DTS End-to-End Test");
    info!("==================");
    info!("Relay: {}", relay_url);
    info!("Mode: {}", mode);
    info!("Quality levels: 1080p (4Mbps), 720p (2Mbps), 480p (800kbps)");
    info!("Duration: {}s", args.duration);
    info!("");

    match mode {
        "subscribe" => run_subscribe_mode(args, relay_url).await,
        "sub-ns-topn" => run_sub_ns_topn_mode(args, relay_url).await,
        _ => {
            error!("Unknown mode: {}. Use 'subscribe' or 'sub-ns-topn'", mode);
            std::process::exit(1);
        }
    }
}

/// Mode 1: Simple SUBSCRIBE flow with SWITCHING-SET-ASSIGNMENT
async fn run_subscribe_mode(args: Args, relay_url: Url) -> anyhow::Result<()> {
    info!("Running SUBSCRIBE mode (direct track subscriptions with DTS params)");
    info!("");

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let test_state = Arc::new(RwLock::new(DtsTestState::new()));

    let mut handles = Vec::new();

    // Start publisher for all quality levels
    let args_clone = args.clone();
    let shutdown_rx = shutdown_tx.subscribe();
    let relay_url_clone = relay_url.clone();

    let handle = tokio::spawn(async move {
        if let Err(e) = run_multi_quality_publisher(args_clone, relay_url_clone, shutdown_rx).await {
            error!("Publisher error: {:#}", e);
        }
    });
    handles.push(handle);

    // Give publisher time to register
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start DTS subscriber
    let args_clone = args.clone();
    let shutdown_rx = shutdown_tx.subscribe();
    let relay_url_clone = relay_url.clone();
    let test_state_clone = test_state.clone();

    let handle = tokio::spawn(async move {
        if let Err(e) = run_dts_subscriber(args_clone, relay_url_clone, test_state_clone, shutdown_rx).await {
            error!("Subscriber error: {:#}", e);
        }
    });
    handles.push(handle);

    // Run test
    info!("Test running for {} seconds...", args.duration);
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    // Shutdown
    info!("Shutting down...");
    let _ = shutdown_tx.send(());

    let shutdown_timeout = Duration::from_secs(5);
    for handle in handles {
        let _ = tokio::time::timeout(shutdown_timeout, handle).await;
    }

    // Print results
    let state = test_state.read().await;
    print_subscribe_results(&state);

    let total_objects: u64 = state.received_from.values().sum();
    if total_objects > 0 {
        info!("");
        info!("DTS_TEST_RESULT: SUCCESS ({} objects received)", total_objects);
        Ok(())
    } else {
        info!("");
        info!("DTS_TEST_RESULT: FAILURE (no objects received)");
        std::process::exit(1);
    }
}

/// Mode 2: SUBSCRIBE_NAMESPACE + Top-N flow with DTS
async fn run_sub_ns_topn_mode(args: Args, relay_url: Url) -> anyhow::Result<()> {
    info!("Running SUBSCRIBE_NAMESPACE + Top-N mode");
    info!("Publishers: {}, Subscribers: {}, Top-N: {}", args.publishers, args.subscribers, args.top_n);
    info!("");

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let test_state = Arc::new(RwLock::new(DtsTestState::new()));

    let mut handles = Vec::new();

    // Start subscribers first (they need to be ready to receive PUBLISH notifications)
    for i in 0..args.subscribers {
        let args_clone = args.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let relay_url_clone = relay_url.clone();
        let test_state_clone = test_state.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_topn_subscriber(i, args_clone, relay_url_clone, test_state_clone, shutdown_rx).await {
                error!("Subscriber {} error: {:#}", i, e);
            }
        });
        handles.push(handle);
    }

    // Give subscribers time to register
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start publishers (each publishes multiple quality tracks)
    for i in 0..args.publishers {
        let args_clone = args.clone();
        let shutdown_rx = shutdown_tx.subscribe();
        let relay_url_clone = relay_url.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_topn_publisher(i, args_clone, relay_url_clone, shutdown_rx).await {
                error!("Publisher {} error: {:#}", i, e);
            }
        });
        handles.push(handle);
    }

    // Run test
    info!("Test running for {} seconds...", args.duration);
    tokio::time::sleep(Duration::from_secs(args.duration)).await;

    // Shutdown
    info!("Shutting down...");
    let _ = shutdown_tx.send(());

    let shutdown_timeout = Duration::from_secs(5);
    for handle in handles {
        let _ = tokio::time::timeout(shutdown_timeout, handle).await;
    }

    // Print results
    let state = test_state.read().await;
    print_topn_results(&state);

    if !state.publishes_received.is_empty() {
        info!("");
        info!("DTS_TEST_RESULT: SUCCESS ({} PUBLISH notifications received)", state.publishes_received.len());
        Ok(())
    } else {
        info!("");
        info!("DTS_TEST_RESULT: FAILURE (no PUBLISH notifications received)");
        std::process::exit(1);
    }
}

fn print_subscribe_results(state: &DtsTestState) {
    info!("");
    info!("Results (SUBSCRIBE mode)");
    info!("-------------------------");
    info!("Objects received by track:");
    for (track, count) in &state.received_from {
        info!("  {}: {} objects", track, count);
    }

    if !state.selection_changes.is_empty() {
        info!("Selection changes:");
        for (time, track) in &state.selection_changes {
            info!("  {:?}: selected {}", time.elapsed(), track);
        }
    }
}

fn print_topn_results(state: &DtsTestState) {
    info!("");
    info!("Results (SUBSCRIBE_NAMESPACE + Top-N mode)");
    info!("------------------------------------------");
    info!("PUBLISH notifications received: {}", state.publishes_received.len());
    for track in &state.publishes_received {
        info!("  - {}", track);
    }

    if !state.received_from.is_empty() {
        info!("Objects received by track:");
        for (track, count) in &state.received_from {
            info!("  {}: {} objects", track, count);
        }
    }
}

async fn connect(args: &Args, relay_url: &Url) -> Result<web_transport::Session> {
    let tls = args.tls.load()?;
    let bind_addr = if relay_url.host_str().map(|h| h.contains(':')).unwrap_or(false) {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let quic = quic::Endpoint::new(quic::Config::new(bind_addr.parse()?, None, tls))?;
    let (session, _cid) = quic.client.connect(relay_url, None).await?;
    Ok(session)
}

// ============================================================================
// Mode 1: SUBSCRIBE flow implementations
// ============================================================================

/// Publisher that publishes multiple quality levels
async fn run_multi_quality_publisher(
    args: Args,
    relay_url: Url,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    debug!("Multi-quality publisher connecting...");

    let session = connect(&args, &relay_url).await.context("failed to connect")?;
    let (session, mut publisher, _subscriber) = Session::connect(session, None)
        .await
        .context("SETUP failed")?;

    let session_handle = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            debug!("Publisher session ended: {}", e);
        }
    });

    tokio::task::yield_now().await;

    let namespace = TrackNamespace::from_utf8_path(&format!("{}/video", args.namespace));

    // Publish namespace
    info!("Publisher: registering namespace {:?}", namespace);
    let publish_ns = publisher.publish_namespace(namespace.clone()).await?;

    tokio::select! {
        result = publish_ns.ok() => {
            result.context("publish namespace failed")?;
        }
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            anyhow::bail!("publish namespace timeout");
        }
    }

    info!("Publisher: namespace registered");

    // Create tracks for each quality level
    let (mut writer, _request, mut reader) = Tracks::new(namespace.clone()).produce();

    let mut track_writers = Vec::new();
    let mut serve_handles = Vec::new();

    for quality in QUALITY_LEVELS {
        let track_writer = writer
            .create(quality.name)
            .ok_or_else(|| anyhow::anyhow!("failed to create track {}", quality.name))?;

        let track_reader = reader
            .subscribe(namespace.clone(), quality.name)
            .ok_or_else(|| anyhow::anyhow!("failed to get track reader for {}", quality.name))?;

        // Need another reader for serve() - get it before publish
        let serve_reader = reader
            .subscribe(namespace.clone(), quality.name)
            .ok_or_else(|| anyhow::anyhow!("failed to get serve reader for {}", quality.name))?;

        // Publish each track
        let mut published = publisher
            .publish(track_reader)
            .await
            .context(format!("failed to send PUBLISH for {}", quality.name))?;

        tokio::select! {
            result = published.ok() => {
                result.context(format!("PUBLISH {} not accepted", quality.name))?;
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                anyhow::bail!("PUBLISH {} timeout", quality.name);
            }
        }

        info!("Publisher: track {} ready", quality.name);

        // Spawn serve task to send data
        let serve_handle = tokio::spawn(async move {
            if let Err(e) = published.serve(serve_reader).await {
                debug!("serve ended: {:?}", e);
            }
        });
        serve_handles.push(serve_handle);

        let subgroups = track_writer.subgroups()?;
        track_writers.push((quality.clone(), subgroups));
    }

    // Send objects for each track
    let group_interval = Duration::from_millis(args.group_interval_ms);
    let mut group_seq: u64 = 1;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Publisher shutting down");
                break;
            }
            _ = tokio::time::sleep(group_interval) => {
                for (quality, subgroups) in &mut track_writers {
                    let subgroup_params = Subgroup {
                        group_id: group_seq,
                        subgroup_id: 0,
                        priority: 0,
                        header_type: None,
                    };
                    let mut subgroup = subgroups.create(subgroup_params)?;

                    let object_size = (quality.bitrate_bps / 8 / 30) as usize;
                    let data = vec![0u8; object_size.min(1024)];

                    let mut object = subgroup.create(data.len(), None)?;
                    object.write(Bytes::from(data))?;
                }

                if group_seq % 10 == 0 {
                    debug!("Publisher: sent group {} for all qualities", group_seq);
                }
                group_seq += 1;
            }
        }
    }

    session_handle.abort();
    debug!("Publisher finished ({} groups)", group_seq);
    Ok(())
}

/// Subscriber that uses DTS to receive adaptive quality
async fn run_dts_subscriber(
    args: Args,
    relay_url: Url,
    test_state: Arc<RwLock<DtsTestState>>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    debug!("DTS subscriber connecting...");

    let session = connect(&args, &relay_url).await.context("failed to connect")?;
    let (session, _publisher, mut subscriber) = Session::connect(session, None)
        .await
        .context("SETUP failed")?;

    let session_handle = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            debug!("Subscriber session ended: {}", e);
        }
    });

    let namespace = TrackNamespace::from_utf8_path(&format!("{}/video", args.namespace));

    // Subscribe to each quality level with SWITCHING-SET-ASSIGNMENT
    info!("Subscriber: subscribing to tracks with DTS params");

    // Keep subscriptions alive for the duration of the test
    let mut subscriptions = Vec::new();
    let mut track_readers = Vec::new();

    for (i, quality) in QUALITY_LEVELS.iter().enumerate() {
        let track_name = quality.name;
        let is_last = i == QUALITY_LEVELS.len() - 1;

        let mut params = KeyValuePairs::new();
        let assignment = SwitchingSetAssignment::new(1, quality.throughput_kbps)
            .with_fraction(10)
            .with_rank(1)
            .with_activate(is_last);
        params.set_switching_set_assignment(&assignment);

        info!(
            "  Subscribing to {} (throughput={}kbps, activate={})",
            track_name, quality.throughput_kbps, is_last
        );

        let (track_writer, track_reader) = Track::new(namespace.clone(), track_name.to_string()).produce();
        let subscribe = subscriber.subscribe_with_params(track_writer, params)?;
        subscriptions.push(subscribe);
        track_readers.push((track_name.to_string(), track_reader));
    }

    info!("Subscriber: {} subscriptions active", subscriptions.len());

    // Spawn readers for each track to count received objects
    let mut reader_handles = Vec::new();
    for (track_name, track_reader) in track_readers {
        let test_state_clone = test_state.clone();
        let track_name_clone = track_name.clone();

        let handle = tokio::spawn(async move {
            let mut objects_received = 0u64;

            // Get subgroups mode
            match track_reader.mode().await {
                Ok(mode) => {
                    match mode {
                        moq_transport::serve::TrackReaderMode::Subgroups(mut subgroups) => {
                            while let Ok(Some(mut subgroup)) = subgroups.next().await {
                                while let Ok(Some(mut obj)) = subgroup.next().await {
                                    // Read object payload
                                    while let Ok(Some(_chunk)) = obj.read().await {
                                        // Count chunks
                                    }
                                    objects_received += 1;

                                    // Update state
                                    let mut state = test_state_clone.write().await;
                                    *state.received_from.entry(track_name_clone.clone()).or_insert(0) += 1;
                                }
                            }
                        }
                        moq_transport::serve::TrackReaderMode::Datagrams(mut datagrams) => {
                            while let Ok(Some(_dgram)) = datagrams.read().await {
                                objects_received += 1;
                                let mut state = test_state_clone.write().await;
                                *state.received_from.entry(track_name_clone.clone()).or_insert(0) += 1;
                            }
                        }
                        _ => {
                            debug!("Unexpected track mode for {}", track_name_clone);
                        }
                    }
                }
                Err(e) => {
                    debug!("Track {} mode error: {}", track_name_clone, e);
                }
            }

            debug!("{}: received {} objects total", track_name_clone, objects_received);
        });
        reader_handles.push(handle);
    }

    // Wait for shutdown or timeout
    let mut status_interval = tokio::time::interval(Duration::from_secs(5));
    let mut elapsed_secs = 0u64;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Subscriber shutting down");
                break;
            }
            _ = status_interval.tick() => {
                elapsed_secs += 5;
                let state = test_state.read().await;
                let total: u64 = state.received_from.values().sum();
                info!("Subscriber: {}s elapsed, {} total objects received", elapsed_secs, total);
                for (track, count) in &state.received_from {
                    info!("  {}: {} objects", track, count);
                }
            }
        }
    }

    // Abort reader tasks
    for handle in reader_handles {
        handle.abort();
    }

    // Keep subscriptions alive until we're done
    drop(subscriptions);

    info!("Subscriber: test complete");

    session_handle.abort();
    debug!("Subscriber finished");
    Ok(())
}

// ============================================================================
// Mode 2: SUBSCRIBE_NAMESPACE + Top-N flow implementations
// ============================================================================

/// Subscriber that uses SUBSCRIBE_NAMESPACE with TRACK_FILTER for top-N
async fn run_topn_subscriber(
    subscriber_id: usize,
    args: Args,
    relay_url: Url,
    test_state: Arc<RwLock<DtsTestState>>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    debug!("Top-N subscriber {} connecting...", subscriber_id);

    let session = connect(&args, &relay_url).await.context("failed to connect")?;
    let (session, _publisher, mut subscriber) = Session::connect(session, None)
        .await
        .context("SETUP failed")?;

    let session_handle = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            debug!("Subscriber {} session ended: {}", subscriber_id, e);
        }
    });

    let namespace_prefix = TrackNamespace::from_utf8_path(&args.namespace);

    // Build TRACK_FILTER parameter
    // Format: key=0x12, value=(property_type << 8) | max_selected
    const TRACK_FILTER_KEY: u64 = 0x12;
    let packed_value = (PROPERTY_VIEWERS << 8) | (args.top_n as u64);
    let mut params = KeyValuePairs::new();
    params.set(KeyValuePair {
        key: TRACK_FILTER_KEY,
        value: Value::IntValue(packed_value),
    });

    info!("Subscriber {}: subscribing to namespace {:?} with top-{} filter (property_type={})",
          subscriber_id, namespace_prefix, args.top_n, PROPERTY_VIEWERS);

    let subscribe_ns = subscriber
        .subscribe_ns_with_params(namespace_prefix.clone(), params)?;

    tokio::select! {
        result = subscribe_ns.ok() => {
            result.context("subscribe namespace failed")?;
        }
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            anyhow::bail!("subscribe namespace timeout");
        }
    }

    info!("Subscriber {}: namespace subscription ready", subscriber_id);

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Subscriber {} shutting down", subscriber_id);
                break;
            }
            result = subscriber.publish_received() => {
                match result {
                    Some(publish_recv) => {
                        let track_name = publish_recv.info.track_name.clone();
                        let full_name = format!("{}/{}", publish_recv.info.track_namespace, track_name);
                        let request_id = publish_recv.info.id;

                        info!("Subscriber {}: received PUBLISH for {}", subscriber_id, full_name);

                        let (writer, _reader) = Track::new(
                            publish_recv.info.track_namespace.clone(),
                            publish_recv.info.track_name.clone(),
                        ).produce();

                        let publish_ok = PublishOk {
                            id: request_id,
                            params: KeyValuePairs::default(),
                        };

                        if let Err(e) = publish_recv.accept(writer, publish_ok) {
                            error!("Subscriber {}: failed to accept PUBLISH: {}", subscriber_id, e);
                        } else {
                            let mut state = test_state.write().await;
                            state.publishes_received.push(full_name);
                        }
                    }
                    None => {
                        debug!("Subscriber {} publish_received closed", subscriber_id);
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }

    session_handle.abort();
    debug!("Subscriber {} finished", subscriber_id);
    Ok(())
}

/// Publisher that publishes tracks with property values for top-N ranking
async fn run_topn_publisher(
    publisher_id: usize,
    args: Args,
    relay_url: Url,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    debug!("Top-N publisher {} connecting...", publisher_id);

    let session = connect(&args, &relay_url).await.context("failed to connect")?;
    let (session, mut publisher, _subscriber) = Session::connect(session, None)
        .await
        .context("SETUP failed")?;

    let session_handle = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            debug!("Publisher {} session ended: {}", publisher_id, e);
        }
    });

    tokio::task::yield_now().await;

    // Each publisher creates its own namespace under the test prefix
    let namespace = TrackNamespace::from_utf8_path(&format!("{}/pub{}", args.namespace, publisher_id));

    info!("Publisher {}: registering namespace {:?}", publisher_id, namespace);
    let publish_ns = publisher.publish_namespace(namespace.clone()).await?;

    tokio::select! {
        result = publish_ns.ok() => {
            result.context("publish namespace failed")?;
        }
        _ = tokio::time::sleep(Duration::from_secs(5)) => {
            anyhow::bail!("publish namespace timeout");
        }
    }

    info!("Publisher {}: namespace registered", publisher_id);

    // Create multiple quality tracks
    let (mut writer, _request, mut reader) = Tracks::new(namespace.clone()).produce();

    let mut track_writers = Vec::new();
    for quality in QUALITY_LEVELS {
        let track_name = quality.name;
        let track_writer = writer
            .create(track_name)
            .ok_or_else(|| anyhow::anyhow!("failed to create track {}", track_name))?;

        let track_reader = reader
            .subscribe(namespace.clone(), track_name)
            .ok_or_else(|| anyhow::anyhow!("failed to get track reader for {}", track_name))?;

        let mut published = publisher
            .publish(track_reader)
            .await
            .context(format!("failed to send PUBLISH for {}", track_name))?;

        tokio::select! {
            result = published.ok() => {
                result.context(format!("PUBLISH {} not accepted", track_name))?;
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                anyhow::bail!("PUBLISH {} timeout", track_name);
            }
        }

        info!("Publisher {}: track {} ready", publisher_id, track_name);

        let subgroups = track_writer.subgroups()?;
        track_writers.push((quality.clone(), subgroups));
    }

    // Simulate viewer count (property value for top-N)
    let initial_viewers = (publisher_id + 1) * 100;
    info!("Publisher {}: initial viewer count = {}", publisher_id, initial_viewers);

    let group_interval = Duration::from_millis(args.group_interval_ms);
    let mut group_seq: u64 = 1;
    let mut current_viewers = initial_viewers as u64;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Publisher {} shutting down", publisher_id);
                break;
            }
            _ = tokio::time::sleep(group_interval) => {
                // Send objects with property extension headers
                for (quality, subgroups) in &mut track_writers {
                    let subgroup_params = Subgroup {
                        group_id: group_seq,
                        subgroup_id: 0,
                        priority: 0,
                        header_type: None,
                    };
                    let mut subgroup = subgroups.create(subgroup_params)?;

                    // Add property extension header for top-N filtering
                    let mut ext_headers = ExtensionHeaders::new();
                    ext_headers.set_intvalue(PROPERTY_VIEWERS, current_viewers);

                    let object_size = (quality.bitrate_bps / 8 / 30) as usize;
                    let data = vec![0u8; object_size.min(1024)];

                    let mut object = subgroup.create(data.len(), Some(ext_headers))?;
                    object.write(Bytes::from(data))?;
                }

                if group_seq % 10 == 0 {
                    debug!("Publisher {}: sent group {} (viewers={})", publisher_id, group_seq, current_viewers);
                }

                // Occasionally vary viewer count to test top-N changes
                if group_seq % 20 == 0 {
                    current_viewers = (current_viewers as i64 + (publisher_id as i64 * 10 - 15)) as u64;
                    current_viewers = current_viewers.max(10);
                    debug!("Publisher {}: viewer count changed to {}", publisher_id, current_viewers);
                }

                group_seq += 1;
            }
        }
    }

    session_handle.abort();
    debug!("Publisher {} finished ({} groups)", publisher_id, group_seq);
    Ok(())
}
