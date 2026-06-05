// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc., Luke Curley, Mike English and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use moq_native_ietf::quic;

use anyhow::Context;

mod cli;
mod clock;
mod privacypass;

use clap::Parser;
use cli::Cli;

use moq_transport::{
    coding::{KeyValuePairs, TrackNamespace},
    serve,
    session::{encode_auth_token, Publisher, Subscriber},
};

/// The main entry point for the MoQ Clock IETF example.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with env filter (respects RUST_LOG environment variable)
    // Default to info level, but suppress quinn's verbose output
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,quinn=warn")),
        )
        .init();

    let config = Cli::parse();
    let tls = config.tls.load()?;

    // Create the QUIC endpoint
    let quic = quic::Endpoint::new(quic::Config::new(config.bind, None, tls)?)?;

    tracing::info!("connecting to server: url={}", config.url);

    // Connect to the server
    let (session, connection_id, transport) = quic.client.connect(&config.url, None).await?;

    tracing::info!(
        "connected with CID: {} (use this to look up qlog/mlog on server)",
        connection_id
    );

    let auth_raw = match &config.auth_token {
        Some(token) => encode_auth_token(config.auth_token_type, token.as_bytes()),
        None => vec![],
    };

    let request_params = if let Some(issuer) = &config.pp_issuer {
        let relay = config.pp_relay.as_ref().unwrap_or(&config.url);
        let action = if config.publish {
            "publish"
        } else {
            "subscribe"
        };
        privacypass::token_params(
            issuer,
            relay,
            action,
            &config.namespace,
            config.tls.disable_verify,
        )
        .await?
    } else {
        KeyValuePairs::default()
    };

    // Depending on whether we are publishing or subscribing, create the appropriate session
    if config.publish {
        // Create the publisher session
        let (session, mut publisher) =
            match Publisher::connect_with_auth(session, transport, auth_raw.clone()).await {
                Ok(session) => session,
                Err(err) if config.pp_issuer.is_some() && config.auth_token.is_none() => {
                    let Some(auth) = privacypass::setup_auth_from_error(
                        config.pp_issuer.as_ref().unwrap(),
                        &err,
                        config.tls.disable_verify,
                    )
                    .await?
                    else {
                        return Err(err).context("failed to create MoQ Transport session");
                    };
                    tracing::info!("retrying MoQ setup with Privacy Pass token");
                    let (session, connection_id, transport) =
                        quic.client.connect(&config.url, None).await?;
                    tracing::info!(
                        "reconnected with CID: {} (use this to look up qlog/mlog on server)",
                        connection_id
                    );
                    Publisher::connect_with_auth(session, transport, auth)
                        .await
                        .context("failed to create MoQ Transport session")?
                }
                Err(err) => return Err(err).context("failed to create MoQ Transport session"),
            };

        if config.datagrams {
            tracing::info!("publishing clock via datagrams");

            let (mut tracks_writer, _, tracks_reader) = serve::Tracks {
                namespace: TrackNamespace::from_utf8_path(&config.namespace),
            }
            .produce();

            let track_writer = tracks_writer.create(&config.track).unwrap();
            let clock_publisher = clock::Publisher::new_datagram(track_writer.datagrams()?);

            tokio::select! {
                res = session.run() => res.context("session error")?,
                res = clock_publisher.run() => res.context("clock error")?,
                res = publisher.announce_with_params(tracks_reader, request_params) => res.context("failed to serve tracks")?,
            }
        } else {
            tracing::info!("publishing clock via streams");

            let (mut tracks_writer, _, tracks_reader) = serve::Tracks {
                namespace: TrackNamespace::from_utf8_path(&config.namespace),
            }
            .produce();

            let track_writer = tracks_writer.create(&config.track).unwrap();
            let clock_publisher = clock::Publisher::new(track_writer.subgroups()?);

            tokio::select! {
                res = session.run() => res.context("session error")?,
                res = clock_publisher.run() => res.context("clock error")?,
                res = publisher.announce_with_params(tracks_reader, request_params) => res.context("failed to serve tracks")?,
            }
        }
    } else {
        // Create the subscriber session
        let (session, mut subscriber) =
            match Subscriber::connect_with_auth(session, transport, auth_raw.clone()).await {
                Ok(session) => session,
                Err(err) if config.pp_issuer.is_some() && config.auth_token.is_none() => {
                    let Some(auth) = privacypass::setup_auth_from_error(
                        config.pp_issuer.as_ref().unwrap(),
                        &err,
                        config.tls.disable_verify,
                    )
                    .await?
                    else {
                        return Err(err).context("failed to create MoQ Transport session");
                    };
                    tracing::info!("retrying MoQ setup with Privacy Pass token");
                    let (session, connection_id, transport) =
                        quic.client.connect(&config.url, None).await?;
                    tracing::info!(
                        "reconnected with CID: {} (use this to look up qlog/mlog on server)",
                        connection_id
                    );
                    Subscriber::connect_with_auth(session, transport, auth)
                        .await
                        .context("failed to create MoQ Transport session")?
                }
                Err(err) => return Err(err).context("failed to create MoQ Transport session"),
            };

        let track_namespace = TrackNamespace::from_utf8_path(&config.namespace);

        if config.track_status {
            // Request a track_status for the clock track (testing purposes only)
            subscriber.track_status(&track_namespace, &config.track);
        }

        let (track_writer, track_reader) =
            serve::Track::new(track_namespace, config.track).produce();

        let clock_subscriber = clock::Subscriber::new(track_reader);

        tokio::select! {
            res = session.run() => res.context("session error")?,
            res = clock_subscriber.run() => res.context("clock error")?,
            res = subscriber.subscribe_with_params(track_writer, request_params) => res.context("failed to subscribe to track")?,
        }
    }

    Ok(())
}
