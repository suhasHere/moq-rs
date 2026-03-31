//! MoQ Chat - Multi-user chat over Media over QUIC
//!
//! This example demonstrates namespace subscriptions for real-time multi-user
//! communication, similar to the quicr-go chat example.
//!
//! # Namespace Structure
//!
//! - Subscribe namespace: `chat/<session-id>` (discover users)
//! - User track: `chat/<session-id>/<user-id>/text` (user messages)
//!
//! # Usage
//!
//! ```bash
//! # Terminal 1 - User Alice
//! moq-chat --server https://localhost:4443 --session room1 --username Alice
//!
//! # Terminal 2 - User Bob
//! moq-chat --server https://localhost:4443 --session room1 --username Bob
//! ```

use moq_native_ietf::quic;

use anyhow::Context;
use clap::Parser;
use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use uuid::Uuid;

mod chat;
mod cli;

use cli::Cli;
use moq_transport::{
    coding::TrackNamespace,
    serve::{self, TracksReader},
    session::{Publisher, SessionError, Subscriber},
};

/// Serve subscriptions to our published tracks.
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

/// Subscribe to another user's messages.
async fn subscribe_to_user(
    subscriber: &mut Subscriber,
    session: &str,
    user_id: &str,
    username: &str,
) -> anyhow::Result<()> {
    let namespace = TrackNamespace::from_utf8_path(&format!("chat/{}/{}", session, user_id));
    let track_name = "text";

    let (track_writer, track_reader) = serve::Track::new(namespace, track_name.to_string()).produce();

    let chat_subscriber = chat::Subscriber::new(
        track_reader,
        user_id.to_string(),
        username.to_string(),
    );

    tokio::select! {
        res = chat_subscriber.run() => res.context("subscriber error")?,
        res = subscriber.subscribe(track_writer) => res.context("failed to subscribe to track")?,
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    // Disable tracing so we don't get a bunch of Quinn spam.
    let tracer = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::WARN)
        .finish();
    tracing::subscriber::set_global_default(tracer).unwrap();

    let config = Cli::parse();
    let tls = config.tls.load()?;

    // Generate user ID and get username
    let user_id = Uuid::new_v4().to_string()[..8].to_string();
    let username = config.username.clone().unwrap_or_else(|| format!("user-{}", &user_id[..4]));

    chat::print_welcome(&config.session, &user_id, &username);

    // Create the QUIC endpoint
    let quic = quic::Endpoint::new(quic::Config::new(config.bind, None, tls))?;

    log::info!("connecting to server: url={}", config.server);

    // Connect to the server
    let (session, connection_id) = quic.client.connect(&config.server, None).await?;

    log::info!(
        "connected with CID: {} (use this to look up qlog/mlog on server)",
        connection_id
    );

    // Create both publisher and subscriber sessions
    // For chat, we need to both publish our messages and subscribe to others
    let (session, mut publisher) = Publisher::connect(session)
        .await
        .context("failed to create MoQ Transport session")?;

    // Create our tracks namespace: chat/<session>/<user-id>
    let namespace_path = config.user_namespace(&user_id);
    let namespace = TrackNamespace::from_utf8_path(&namespace_path);

    // Set up the track for our messages
    let (mut tracks_writer, _, tracks_reader) = serve::Tracks {
        namespace: namespace.clone(),
    }
    .produce();

    let track_name = "text";

    // Spawn stdin reader
    let stdin_rx = chat::spawn_stdin_reader();

    if config.datagrams {
        log::info!("publishing chat via datagrams");

        let track_writer = tracks_writer.create(track_name).unwrap();
        let chat_publisher = chat::DatagramPublisher::new(
            track_writer.datagrams()?,
            user_id.clone(),
            username.clone(),
        );

        let publish_ns = publisher
            .publish_namespace(namespace)
            .await
            .context("failed to register namespace")?;

        tokio::select! {
            res = session.run() => res.context("session error")?,
            res = chat_publisher.run(stdin_rx) => res.context("chat publisher error")?,
            res = serve_subscriptions(publisher, tracks_reader) => res.context("failed to serve tracks")?,
            res = publish_ns.closed() => res.context("namespace closed")?,
        }
    } else {
        log::info!("publishing chat via streams");

        let track_writer = tracks_writer.create(track_name).unwrap();
        let chat_publisher = chat::StreamPublisher::new(
            track_writer.subgroups()?,
            user_id.clone(),
            username.clone(),
        );

        let publish_ns = publisher
            .publish_namespace(namespace)
            .await
            .context("failed to register namespace")?;

        tokio::select! {
            res = session.run() => res.context("session error")?,
            res = chat_publisher.run(stdin_rx) => res.context("chat publisher error")?,
            res = serve_subscriptions(publisher, tracks_reader) => res.context("failed to serve tracks")?,
            res = publish_ns.closed() => res.context("namespace closed")?,
        }
    }

    Ok(())
}
