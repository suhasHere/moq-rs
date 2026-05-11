use std::{future::Future, net, path::PathBuf, pin::Pin, sync::Arc};

use anyhow::Context;

use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use moq_native_ietf::quic::{self, AcceptedConnection, CongestionControl, Endpoint};
use url::Url;

use crate::{
    Consumer, Coordinator, DtsServiceHandle, DtsService, Locals, Producer,
    QuicStatsConsumer, QuicStatsReporter, QuicStatsSender,
    Remotes, RemotesConsumer, RemotesProducer, Session,
    SubscriberRegistry, TieBreakPolicy, quic_stats_channel,
};

// A type alias for boxed future
type ServerFuture = Pin<
    Box<
        dyn Future<
            Output = (
                anyhow::Result<AcceptedConnection>,
                quic::Server,
            ),
        >,
    >,
>;

/// Configuration for the relay.
pub struct RelayConfig {
    /// Listen on this address
    pub bind: Option<net::SocketAddr>,

    /// Optional list of endpoints if provided, we won't use bind
    pub endpoints: Vec<Endpoint>,

    /// The TLS configuration.
    pub tls: moq_native_ietf::tls::Config,

    /// Directory to write qlog files (one per connection)
    pub qlog_dir: Option<PathBuf>,

    /// Directory to write mlog files (one per connection)
    pub mlog_dir: Option<PathBuf>,

    /// Forward all announcements to the (optional) URL.
    pub announce: Option<Url>,

    /// Our hostname which we advertise to other origins.
    /// We use QUIC, so the certificate must be valid for this address.
    pub node: Option<Url>,

    /// The coordinator for namespace/track registration and discovery.
    pub coordinator: Arc<dyn Coordinator>,

    /// Enable TopN event logging for visualization
    /// Logs JSON events to stdout that can be used to generate timeline SVGs
    pub topn_log: bool,

    /// Tie-break policy for top-N filtering
    pub tie_break_policy: TieBreakPolicy,

    /// Enable DTS (Dynamic Track Switching) for ABR video
    pub dts_enabled: bool,

    /// Congestion control algorithm
    pub cc: CongestionControl,
}

/// MoQ Relay server.
pub struct Relay {
    quic_endpoints: Vec<Endpoint>,
    announce_url: Option<Url>,
    mlog_dir: Option<PathBuf>,
    locals: Locals,
    remotes: Option<(RemotesProducer, RemotesConsumer)>,
    coordinator: Arc<dyn Coordinator>,
    subscriber_registry: SubscriberRegistry,
    dts_service: Option<DtsServiceHandle>,
    quic_stats_sender: Option<QuicStatsSender>,
}

impl Relay {
    pub fn new(config: RelayConfig) -> anyhow::Result<Self> {
        if config.bind.is_some() && !config.endpoints.is_empty() {
            anyhow::bail!("cannot specify both bind and endpoints");
        }

        let endpoints = if let Some(bind) = config.bind {
            let endpoint = quic::Endpoint::new(quic::Config::new(
                bind,
                config.qlog_dir.clone(),
                config.tls.clone(),
            ).with_cc(config.cc))?;
            vec![endpoint]
        } else {
            config.endpoints
        };

        if endpoints.is_empty() {
            anyhow::bail!("no endpoints available to start the server");
        }

        // Validate mlog directory if provided
        if let Some(mlog_dir) = &config.mlog_dir {
            if !mlog_dir.exists() {
                anyhow::bail!("mlog directory does not exist: {}", mlog_dir.display());
            }
            if !mlog_dir.is_dir() {
                anyhow::bail!("mlog path is not a directory: {}", mlog_dir.display());
            }
            log::info!("mlog output enabled: {}", mlog_dir.display());
        }

        let locals = Locals::new();

        // FIXME(itzmanish): have a generic filter to find endpoints for forward, remote etc.
        let remote_clients = endpoints
            .iter()
            .map(|endpoint| endpoint.client.clone())
            .collect::<Vec<_>>();

        // Create remote manager - uses coordinator for namespace lookups
        let remotes = Remotes {
            coordinator: config.coordinator.clone(),
            quic: remote_clients[0].clone(),
        }
        .produce();

        // Create subscriber registry for SUBSCRIBE_NAMESPACE tracking
        let subscriber_registry = if config.topn_log {
            log::info!("TopN event logging enabled - JSON events will be written to stdout");
            log::info!("TopN tie-break policy: {:?}", config.tie_break_policy);
            SubscriberRegistry::with_config(true, config.tie_break_policy)
        } else {
            SubscriberRegistry::with_config(false, config.tie_break_policy)
        };

        // Create DTS service and QUIC stats channel if enabled
        let (dts_service, quic_stats_sender) = if config.dts_enabled {
            log::info!("DTS (Dynamic Track Switching) enabled for ABR video");
            let dts = Arc::new(DtsService::new());
            let (sender, receiver) = quic_stats_channel();

            // Spawn the QUIC stats consumer task
            let consumer = QuicStatsConsumer::new(receiver, dts.clone());
            tokio::spawn(consumer.run());

            (Some(dts), Some(sender))
        } else {
            (None, None)
        };

        Ok(Self {
            quic_endpoints: endpoints,
            announce_url: config.announce,
            mlog_dir: config.mlog_dir,
            locals,
            remotes: Some(remotes),
            coordinator: config.coordinator,
            subscriber_registry,
            dts_service,
            quic_stats_sender,
        })
    }

    /// Run the relay server.
    pub async fn run(self) -> anyhow::Result<()> {
        let mut tasks = FuturesUnordered::new();

        // Split remotes producer/consumer and spawn producer task
        let remotes = self.remotes.map(|(producer, consumer)| {
            tasks.push(producer.run().boxed());
            consumer
        });

        // Start the forwarder, if any
        let forward_producer = if let Some(url) = &self.announce_url {
            log::info!("forwarding announces to {}", url);

            // Establish a QUIC connection to the forward URL
            let (session, _quic_client_initial_cid) = self.quic_endpoints[0]
                .client
                .connect(url, None)
                .await
                .context("failed to establish forward connection")?;

            // Create the MoQ session over the connection
            let (session, publisher, subscriber) =
                moq_transport::session::Session::connect(session, None)
                    .await
                    .context("failed to establish forward session")?;

            // Create a normal looking session, except we never forward or register announces.
            let coordinator = self.coordinator.clone();
            let session = Session {
                session,
                producer: Some(Producer::new(
                    publisher,
                    self.locals.clone(),
                    remotes.clone(),
                )),
                consumer: Some(Consumer::new(
                    subscriber,
                    self.locals.clone(),
                    coordinator,
                    None,
                )),
            };

            let forward_producer = session.producer.clone();

            tasks.push(async move { session.run().await.context("forwarding failed") }.boxed());

            forward_producer
        } else {
            None
        };

        let servers: Vec<quic::Server> = self
            .quic_endpoints
            .into_iter()
            .map(|endpoint| {
                endpoint
                    .server
                    .context("missing TLS certificate for server")
            })
            .collect::<anyhow::Result<_>>()?;

        // This will hold the futures for all our listening servers.
        let mut accepts: FuturesUnordered<ServerFuture> = FuturesUnordered::new();
        for mut server in servers {
            log::info!("listening on {}", server.local_addr()?);

            // Create a future, box it, and push it to the collection.
            accepts.push(
                async move {
                    let conn = server.accept().await.ok_or_else(|| anyhow::anyhow!("accept failed"));
                    (conn, server)
                }
                .boxed(),
            );
        }

        loop {
            tokio::select! {
                // This branch polls all the `accept` futures concurrently.
                Some((conn_result, mut server)) = accepts.next() => {
                    // An accept operation has completed.
                    // First, immediately queue up the next accept() call for this server.
                    accepts.push(
                        async move {
                            let conn = server.accept().await.ok_or_else(|| anyhow::anyhow!("accept failed"));
                            (conn, server)
                        }
                        .boxed(),
                    );

                    let accepted = conn_result.context("failed to accept QUIC connection")?;
                    let conn = accepted.session;
                    let connection_id = accepted.connection_id;
                    let quinn_connection = accepted.quinn_connection;

                    // Construct mlog path from connection ID if mlog directory is configured
                    let mlog_path = self.mlog_dir.as_ref()
                        .map(|dir| dir.join(format!("{}_server.mlog", connection_id)));

                    let locals = self.locals.clone();
                    let remotes = remotes.clone();
                    let forward = forward_producer.clone();
                    let coordinator = self.coordinator.clone();
                    let subscriber_registry = self.subscriber_registry.clone();
                    let dts_service = self.dts_service.clone();
                    let quic_stats_sender = self.quic_stats_sender.clone();

                    // Spawn a new task to handle the connection
                    tasks.push(async move {
                        // Create the MoQ session over the connection (setup handshake etc)
                        let (session, publisher, subscriber) = match moq_transport::session::Session::accept(conn, mlog_path).await {
                            Ok(session) => session,
                            Err(err) => {
                                log::warn!("failed to accept MoQ session: {}", err);
                                return Ok(());
                            }
                        };

                        // Create our MoQ relay session
                        // Use connection_id hash as session_id for self-exclusion in pub/sub
                        use std::hash::{Hash, Hasher};
                        let session_id = {
                            let mut hasher = std::collections::hash_map::DefaultHasher::new();
                            connection_id.hash(&mut hasher);
                            hasher.finish()
                        };

                        // Register session with DTS service if enabled
                        if let Some(ref dts) = dts_service {
                            dts.register_subscriber(session_id);
                        }

                        // Spawn periodic QUIC stats reporter for this session if DTS is enabled
                        let stats_task = if let Some(ref sender) = quic_stats_sender {
                            let reporter = QuicStatsReporter::new(sender.clone(), session_id);
                            let quinn_conn = quinn_connection.clone();
                            log::info!("DTS: starting QUIC stats reporter for session {}", session_id);
                            Some(tokio::spawn(async move {
                                let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
                                loop {
                                    interval.tick().await;
                                    let stats = quinn_conn.stats();
                                    let path = stats.path;
                                    log::debug!(
                                        "DTS: QUIC stats for session: rtt={:?}, cwnd={}",
                                        path.rtt, path.cwnd
                                    );
                                    reporter.report(
                                        path.rtt,
                                        path.cwnd,
                                        path.congestion_events,
                                        path.lost_packets,
                                        stats.udp_tx.bytes,
                                    );
                                }
                            }))
                        } else {
                            None
                        };

                        let moq_session = session;
                        let session = Session {
                            session: moq_session,
                            producer: publisher.map(|publisher| {
                                Producer::with_registry_and_dts(
                                    publisher,
                                    locals.clone(),
                                    remotes,
                                    subscriber_registry.clone(),
                                    session_id,
                                    dts_service.clone(),
                                )
                            }),
                            consumer: subscriber.map(|subscriber| {
                                Consumer::with_registry_and_dts(
                                    subscriber,
                                    locals,
                                    coordinator,
                                    forward,
                                    subscriber_registry,
                                    session_id,
                                    dts_service.clone(),
                                )
                            }),
                        };

                        if let Err(err) = session.run().await {
                            log::warn!("failed to run MoQ session: {}", err);
                        }

                        // Stop the stats reporter task when session ends
                        if let Some(task) = stats_task {
                            task.abort();
                        }

                        Ok(())
                    }.boxed());
                },
                res = tasks.next(), if !tasks.is_empty() => res.unwrap()?,
            }
        }
    }
}
