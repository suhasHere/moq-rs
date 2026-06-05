// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc., Luke Curley, Mike English and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

mod api_coordinator;
mod file_coordinator;

use std::sync::Arc;
use std::time::Duration;
use std::{net, path::PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use clap::Parser;
use serde::Deserialize;
use url::Url;

use api_coordinator::{ApiCoordinator, ApiCoordinatorConfig};
use file_coordinator::FileCoordinator;
use moq_relay_ietf::{Coordinator, Relay, RelayConfig, Web, WebConfig};

#[derive(Parser, Clone)]
pub struct Cli {
    /// Listen on this address
    #[arg(long, default_value = "[::]:443")]
    pub bind: net::SocketAddr,

    /// The TLS configuration.
    #[command(flatten)]
    pub tls: moq_native_ietf::tls::Args,

    /// Directory to write qlog files (one per connection)
    #[arg(long)]
    pub qlog_dir: Option<PathBuf>,

    /// Directory to write mlog files (one per connection)
    #[arg(long)]
    pub mlog_dir: Option<PathBuf>,

    /// Forward all announces to the provided server for authentication/routing.
    /// If not provided, the relay accepts every unique announce.
    #[arg(long)]
    pub announce: Option<Url>,

    /// The URL of the moq-api server in order to run a cluster.
    /// Must be used in conjunction with --node to advertise the origin
    #[arg(long)]
    pub api: Option<Url>,

    /// The hostname that we advertise to other origins.
    /// The provided certificate must be valid for this address.
    #[arg(long)]
    pub node: Option<Url>,

    /// Enable development mode.
    /// This hosts a HTTPS web server via TCP to serve the fingerprint of the certificate.
    #[arg(long)]
    pub dev: bool,

    /// Serve qlog files over HTTPS at /qlog/:cid
    /// Requires --dev to enable the web server. Only serves files by exact CID - no index.
    #[arg(long)]
    pub qlog_serve: bool,

    /// Serve mlog files over HTTPS at /mlog/:cid
    /// Requires --dev to enable the web server. Only serves files by exact CID - no index.
    #[arg(long)]
    pub mlog_serve: bool,

    /// Path to the shared coordinator file for multi-relay coordination.
    /// Multiple relay instances can share namespace/track registration via this file.
    /// User doesn't have to explicitly create and populate anything. This path will be
    /// used by file coordinator to store namespace/track registration information.
    /// User need to make sure if multiple relay's are being used all of them have same path
    /// to this file.
    #[arg(long, default_value = "/tmp/moq-coordinator.json")]
    pub coordinator_file: PathBuf,

    /// URL of the moq-api server for coordination (e.g., "http://localhost:8080").
    /// When specified, uses moq-api HTTP server instead of file-based coordination.
    /// This is useful when running a cluster of relays with a centralized API server.
    #[arg(long)]
    pub api_url: Option<Url>,

    /// TTL in seconds for namespace registrations in the API.
    /// Only used when --api-url is specified.
    #[arg(long, default_value = "600")]
    pub api_ttl: u64,

    /// Address to expose Prometheus metrics on (e.g., "127.0.0.1:9090").
    /// Requires the `metrics-prometheus` feature to be enabled.
    /// When set, serves metrics at http://<addr>/metrics
    #[arg(long)]
    pub metrics_addr: Option<net::SocketAddr>,

    /// Shared secret for token-type-0 auth (simple pre-shared key).
    /// Clients must pass --auth-token-type 0 --auth-token <secret>.
    #[arg(long, env = "MOQ_AUTH_SHARED_SECRET")]
    pub auth_shared_secret: Option<String>,

    /// Enable Privacy Pass public token auth using issuer directory keys.
    #[arg(long)]
    pub auth_privacypass: bool,

    /// Privacy Pass issuer base URL.
    #[arg(long, default_value = "https://demo-pat.issuer.cloudflare.com")]
    pub pp_issuer: Url,

    /// Require Privacy Pass during SETUP, not only operation requests.
    #[arg(long, default_value_t = false)]
    pub pp_setup_required: bool,

    /// Privacy Pass challenge TTL in seconds.
    #[arg(long, default_value_t = 300)]
    pub pp_challenge_ttl: u64,
}

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

    let cli = Cli::parse();

    // Initialize Prometheus metrics exporter if --metrics-addr is provided
    #[cfg(feature = "metrics-prometheus")]
    if let Some(metrics_addr) = cli.metrics_addr {
        use metrics_exporter_prometheus::PrometheusBuilder;

        // Configure histogram buckets for subscribe latency (1ms to 10s)
        let subscribe_latency_buckets = vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0, 10.0,
        ];

        PrometheusBuilder::new()
            .with_http_listener(metrics_addr)
            .set_buckets_for_metric(
                metrics_exporter_prometheus::Matcher::Full(
                    "moq_relay_subscribe_latency_seconds".to_string(),
                ),
                &subscribe_latency_buckets,
            )?
            .install()
            .expect("failed to install Prometheus metrics exporter");

        // Register metric descriptions (shows as # HELP in Prometheus output)
        moq_relay_ietf::metrics::describe_metrics();

        tracing::info!(
            "metrics exporter listening on http://{}/metrics",
            metrics_addr
        );
    }

    #[cfg(not(feature = "metrics-prometheus"))]
    if cli.metrics_addr.is_some() {
        tracing::warn!(
            "--metrics-addr was provided but the metrics-prometheus feature is not enabled. \
             Rebuild with --features metrics-prometheus to enable the Prometheus exporter."
        );
    }

    let tls = cli.tls.load()?;

    if tls.server.is_none() {
        anyhow::bail!("missing TLS certificates");
    }

    // Determine qlog directory for both relay and web server
    let qlog_dir_for_relay = cli.qlog_dir.clone();
    let qlog_dir_for_web = if cli.qlog_serve {
        cli.qlog_dir.clone()
    } else {
        None
    };

    // Determine mlog directory for both relay and web server
    let mlog_dir_for_relay = cli.mlog_dir.clone();
    let mlog_dir_for_web = if cli.mlog_serve {
        cli.mlog_dir.clone()
    } else {
        None
    };

    // Build the relay URL from the node or bind address
    let relay_url = cli
        .node
        .clone()
        .unwrap_or_else(|| Url::parse(&format!("https://{}", cli.bind)).unwrap());

    // Create the coordinator based on CLI arguments
    // Priority: api-url > file coordinator
    let coordinator: Arc<dyn Coordinator> = if let Some(api_url) = &cli.api_url {
        let config = ApiCoordinatorConfig::new(api_url.clone(), relay_url).with_ttl(cli.api_ttl);
        let api_coordinator = ApiCoordinator::new(config);
        tracing::info!("using API coordinator: {}", api_url);
        Arc::new(api_coordinator)
    } else {
        tracing::info!("using file coordinator: {}", cli.coordinator_file.display());
        Arc::new(FileCoordinator::new(&cli.coordinator_file, relay_url))
    };

    // Build the auth hook if configured.
    let auth_config = build_auth_hook(&cli).await?;

    // Create a QUIC server for media.
    let relay = Relay::new(RelayConfig {
        tls: tls.clone(),
        bind: Some(cli.bind),
        endpoints: vec![],
        qlog_dir: qlog_dir_for_relay,
        mlog_dir: mlog_dir_for_relay,
        node: cli.node,
        announce: cli.announce,
        coordinator,
        auth_hook: auth_config.hook.clone(),
        auth_setup_challenge_reason: auth_config.pp_setup_challenge_reason.clone(),
    })?;

    if cli.dev {
        // Create a web server too.
        // Currently this only contains the certificate fingerprint (for development only).
        let web = Web::new(WebConfig {
            bind: cli.bind,
            tls,
            qlog_dir: qlog_dir_for_web,
            mlog_dir: mlog_dir_for_web,
            pp_challenges: auth_config.pp_challenges.clone(),
            pp_issuer_name: auth_config.pp_issuer_name.clone(),
        });

        tokio::spawn(async move {
            web.run().await.expect("failed to run web server");
        });
    }

    relay.run().await
}

struct AuthConfig {
    hook: Option<Arc<dyn moq_auth::AuthHook>>,
    pp_challenges: Option<Arc<moq_auth_privacypass::ChallengeRegistry>>,
    pp_issuer_name: Option<String>,
    pp_setup_challenge_reason: Option<String>,
}

async fn build_auth_hook(cli: &Cli) -> anyhow::Result<AuthConfig> {
    if cli.auth_shared_secret.is_some() && cli.auth_privacypass {
        anyhow::bail!("--auth-shared-secret and --auth-privacypass are mutually exclusive");
    }

    if let Some(ref secret) = cli.auth_shared_secret {
        tracing::info!("shared-secret auth enabled (token type 0)");
        return Ok(AuthConfig {
            hook: Some(Arc::new(moq_auth::KeyValueAuthHook::new(
                secret.as_bytes().to_vec(),
            ))),
            pp_challenges: None,
            pp_issuer_name: None,
            pp_setup_challenge_reason: None,
        });
    }

    if cli.auth_privacypass {
        let public_keys = load_privacypass_public_keys(cli.pp_issuer.clone()).await?;
        let hook = moq_auth_privacypass::PrivacyPassAuthHook::demo(
            public_keys,
            Duration::from_secs(cli.pp_challenge_ttl),
        )
        .with_setup_required(cli.pp_setup_required);
        let challenges = hook.challenges();
        let issuer_name = cli.pp_issuer.host_str().map(ToString::to_string);
        tracing::info!(issuer = %cli.pp_issuer, setup_required = cli.pp_setup_required, "Privacy Pass auth enabled");
        return Ok(AuthConfig {
            hook: Some(Arc::new(hook)),
            pp_challenges: Some(challenges),
            pp_setup_challenge_reason: issuer_name
                .as_deref()
                .map(moq_auth_privacypass::setup_challenge_reason)
                .transpose()?,
            pp_issuer_name: issuer_name,
        });
    }

    Ok(AuthConfig {
        hook: None,
        pp_challenges: None,
        pp_issuer_name: None,
        pp_setup_challenge_reason: None,
    })
}

#[derive(Debug, Deserialize)]
struct IssuerDirectory {
    #[serde(rename = "token-keys")]
    token_keys: Vec<IssuerTokenKey>,
}

#[derive(Debug, Deserialize)]
struct IssuerTokenKey {
    #[serde(rename = "token-type")]
    token_type: u16,
    #[serde(rename = "token-key")]
    token_key: String,
}

async fn load_privacypass_public_keys(
    issuer: Url,
) -> anyhow::Result<Arc<moq_auth_privacypass::PublicKeyStore>> {
    let directory_url = issuer.join("/.well-known/private-token-issuer-directory")?;
    let directory: IssuerDirectory = reqwest::get(directory_url.clone())
        .await?
        .error_for_status()?
        .json()
        .await?;

    let store = Arc::new(moq_auth_privacypass::PublicKeyStore::default());
    let mut loaded = 0usize;
    for key in directory.token_keys {
        if key.token_type != moq_auth_privacypass::PRIVACY_PASS_PUBLIC_TOKEN_TYPE {
            continue;
        }
        let der = URL_SAFE_NO_PAD.decode(key.token_key.as_bytes())?;
        let public_key = moq_auth_privacypass::public_key_from_spki_der(&der)?;
        store.insert_public_key(public_key).await?;
        loaded += 1;
    }

    if loaded == 0 {
        anyhow::bail!("issuer directory did not contain Privacy Pass public token keys");
    }

    tracing::info!(issuer = %issuer, keys = loaded, "loaded Privacy Pass issuer keys");
    Ok(store)
}
