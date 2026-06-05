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
    session::{encode_auth_token, Publisher, SessionError, Subscriber},
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

    let request_params = KeyValuePairs::default();

    // Depending on whether we are publishing or subscribing, create the appropriate session
    if config.publish {
        // Create the publisher session
        let (session, publisher) =
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
                res = announce_with_retry(publisher, tracks_reader, request_params, config.pp_issuer.as_ref(), config.tls.disable_verify) => res.context("failed to serve tracks")?,
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
                res = announce_with_retry(publisher, tracks_reader, request_params, config.pp_issuer.as_ref(), config.tls.disable_verify) => res.context("failed to serve tracks")?,
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

        let mut session_task = tokio::spawn(async move { session.run().await });

        let track_namespace = TrackNamespace::from_utf8_path(&config.namespace);

        if config.track_status {
            // Request a track_status for the clock track (testing purposes only)
            subscriber.track_status(&track_namespace, &config.track);
        }

        let (track_writer, track_reader) =
            serve::Track::new(track_namespace.clone(), config.track.clone()).produce();

        let subscribe = match subscriber
            .subscribe_open_with_params(track_writer, request_params)
            .await
        {
            Ok(subscribe) => subscribe,
            Err(err) if config.pp_issuer.is_some() && config.auth_token.is_none() => {
                let Some(params) = privacypass::token_params_from_error(
                    config.pp_issuer.as_ref().unwrap(),
                    &err,
                    config.tls.disable_verify,
                )
                .await?
                else {
                    return Err(err).context("failed to subscribe to track");
                };
                tracing::info!("retrying subscribe with Privacy Pass token");
                let (track_writer, retry_reader) =
                    serve::Track::new(track_namespace, config.track).produce();
                let subscribe = subscriber
                    .subscribe_open_with_params(track_writer, params)
                    .await
                    .context("failed to subscribe to track")?;
                let clock_subscriber = clock::Subscriber::new(retry_reader);
                tokio::select! {
                    res = &mut session_task => res.context("session task panicked")?.context("session error")?,
                    res = clock_subscriber.run() => res.context("clock error")?,
                    res = subscribe.closed() => res.context("subscription closed")?,
                }
                return Ok(());
            }
            Err(err) => return Err(err).context("failed to subscribe to track"),
        };

        let clock_subscriber = clock::Subscriber::new(track_reader);

        tokio::select! {
            res = &mut session_task => res.context("session task panicked")?.context("session error")?,
            res = clock_subscriber.run() => res.context("clock error")?,
            res = subscribe.closed() => res.context("subscription closed")?,
        }
    }

    Ok(())
}

async fn announce_with_retry(
    mut publisher: Publisher,
    tracks: serve::TracksReader,
    params: KeyValuePairs,
    issuer: Option<&url::Url>,
    disable_verify: bool,
) -> anyhow::Result<()> {
    match publisher.announce_with_params(tracks.clone(), params).await {
        Ok(()) => Ok(()),
        Err(SessionError::Serve(err)) if issuer.is_some() => {
            let Some(params) =
                privacypass::token_params_from_error(issuer.unwrap(), &err, disable_verify).await?
            else {
                return Err(SessionError::Serve(err).into());
            };
            tracing::info!("retrying publish with Privacy Pass token");
            publisher.announce_with_params(tracks, params).await?;
            Ok(())
        }
        Err(err) => Err(err.into()),
    }
}
