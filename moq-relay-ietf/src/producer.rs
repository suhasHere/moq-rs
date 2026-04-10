use std::sync::Arc;
use std::time::Duration;

use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use moq_transport::{
    coding::{KeyValuePairs, TrackNamespace},
    message::{self, TrackFilter},
    serve::{ServeError, TracksReader},
    session::{
        PublishNamespace, Publisher, SessionError, SubscribeNamespaceReceived,
        Subscribed, TrackStatusRequested,
    },
};

/// Type alias for object filter function
pub type ObjectFilterFn = Arc<dyn Fn(&[(u64, u64)]) -> bool + Send + Sync>;

use crate::filter::{FilterPipeline, PublisherId, ScalableTopNFilter, TopNConfig, TopNFilter};
use crate::{Locals, RemotesConsumer, SessionPublisherTracker, SubscriberRegistry};

/// Producer of tracks to a remote Subscriber
#[derive(Clone)]
pub struct Producer {
    publisher: Publisher,
    locals: Locals,
    remotes: Option<RemotesConsumer>,
    filter_pipeline: Option<Arc<FilterPipeline>>,
    scalable_topn_filter: Option<Arc<ScalableTopNFilter>>,
    subscriber_registry: Option<SubscriberRegistry>,
    /// Shared tracker to know if this session is also a publisher
    publisher_tracker: Option<SessionPublisherTracker>,
}

impl Producer {
    pub fn new(publisher: Publisher, locals: Locals, remotes: Option<RemotesConsumer>) -> Self {
        Self {
            publisher,
            locals,
            remotes,
            filter_pipeline: None,
            scalable_topn_filter: None,
            subscriber_registry: None,
            publisher_tracker: None,
        }
    }

    /// Creates a producer with a filter pipeline.
    pub fn with_filter_pipeline(
        publisher: Publisher,
        locals: Locals,
        remotes: Option<RemotesConsumer>,
        filter_pipeline: Arc<FilterPipeline>,
    ) -> Self {
        Self {
            publisher,
            locals,
            remotes,
            filter_pipeline: Some(filter_pipeline),
            scalable_topn_filter: None,
            subscriber_registry: None,
            publisher_tracker: None,
        }
    }

    /// Creates a producer with a subscriber registry.
    pub fn with_registry(
        publisher: Publisher,
        locals: Locals,
        remotes: Option<RemotesConsumer>,
        subscriber_registry: SubscriberRegistry,
    ) -> Self {
        // Get the scalable filter from the registry if available
        let scalable_topn_filter = subscriber_registry.topn_filter().cloned();

        Self {
            publisher,
            locals,
            remotes,
            filter_pipeline: None,
            scalable_topn_filter,
            subscriber_registry: Some(subscriber_registry),
            publisher_tracker: None,
        }
    }

    /// Creates a producer with registry and shared publisher tracker for self-exclusion.
    ///
    /// The publisher_tracker should be shared with the Consumer for the same session.
    /// This enables self-exclusion: when this session subscribes, they won't see
    /// their own published tracks in the top-N selection.
    pub fn with_registry_and_tracker(
        publisher: Publisher,
        locals: Locals,
        remotes: Option<RemotesConsumer>,
        subscriber_registry: SubscriberRegistry,
        publisher_tracker: SessionPublisherTracker,
    ) -> Self {
        let scalable_topn_filter = subscriber_registry.topn_filter().cloned();

        Self {
            publisher,
            locals,
            remotes,
            filter_pipeline: None,
            scalable_topn_filter,
            subscriber_registry: Some(subscriber_registry),
            publisher_tracker: Some(publisher_tracker),
        }
    }

    /// Sets the filter pipeline.
    pub fn set_filter_pipeline(&mut self, pipeline: Arc<FilterPipeline>) {
        self.filter_pipeline = Some(pipeline);
    }

    pub async fn publish_namespace(
        &mut self,
        tracks: TracksReader,
    ) -> Result<PublishNamespace, SessionError> {
        self.publisher
            .publish_namespace(tracks.namespace.clone())
            .await
    }

    /// Creates a TopNFilter from a TrackFilter (from SUBSCRIBE_NAMESPACE).
    ///
    /// If TrackFilter is present, creates a TopNFilter using its settings.
    /// Falls back to global filter_pipeline settings for any missing values.
    fn create_topn_filter_from_track_filter(
        track_filter: Option<&TrackFilter>,
        filter_pipeline: Option<&Arc<FilterPipeline>>,
    ) -> Option<Arc<TopNFilter>> {
        // If we have a TrackFilter from SUBSCRIBE_NAMESPACE, use its settings
        if let Some(tf) = track_filter {
            let config = TopNConfig {
                n: tf.max_tracks_selected as usize,
                metric_extension_type: tf.property_type,
                decay_after: Duration::from_millis(tf.timeout_ms),
                // Use a reasonable recompute interval (half of decay or 500ms, whichever is smaller)
                recompute_interval: Duration::from_millis(
                    (tf.timeout_ms / 2).max(100).min(500)
                ),
                higher_is_better: true,
            };
            return Some(Arc::new(TopNFilter::new(config)));
        }

        // Fall back to global filter pipeline if enabled
        if let Some(pipeline) = filter_pipeline {
            if pipeline.is_topn_enabled() {
                // Clone the global config and create a new filter instance
                let config = pipeline.config().topn_config.clone();
                return Some(Arc::new(TopNFilter::new(config)));
            }
        }

        None
    }

    pub async fn run(self) -> Result<(), SessionError> {
        //let mut tasks = FuturesUnordered::new();
        let mut tasks: FuturesUnordered<futures::future::BoxFuture<'static, ()>> =
            FuturesUnordered::new();

        loop {
            let mut publisher_subscribed = self.publisher.clone();
            let mut publisher_track_status = self.publisher.clone();
            let mut publisher_subscribe_ns = self.publisher.clone();

            tokio::select! {
                // Handle a new subscribe request
                Some(subscribed) = publisher_subscribed.subscribed() => {
                    let this = self.clone();

                    // Spawn a new task to handle the subscribe
                    tasks.push(async move {
                        let info = subscribed.clone();
                        log::info!("serving subscribe: {:?}", info);

                        // Serve the subscribe request
                        if let Err(err) = this.serve_subscribe(subscribed).await {
                            log::warn!("failed serving subscribe: {:?}, error: {}", info, err);
                        }
                    }.boxed())
                },
                // Handle a new track_status request
                Some(track_status_requested) = publisher_track_status.track_status_requested() => {
                    let this = self.clone();

                    // Spawn a new task to handle the track_status request
                    tasks.push(async move {
                        let info = track_status_requested.request_msg.clone();
                        log::info!("serving track_status: {:?}", info);

                        // Serve the track_status request
                        if let Err(err) = this.serve_track_status(track_status_requested).await {
                            log::warn!("failed serving track_status: {:?}, error: {}", info, err)
                        }
                    }.boxed())
                },
                Some(subscribe_ns) = publisher_subscribe_ns.subscribe_namespace_received() => {
                    let this = self.clone();

                    tasks.push(async move {
                        let info = subscribe_ns.info.clone();
                        log::info!("serving subscribe_namespace: {:?}", info);

                        if let Err(err) = this.serve_subscribe_namespace(subscribe_ns).await {
                            log::warn!("failed serving subscribe_namespace: {:?}, error: {}", info, err)
                        }
                    }.boxed())
                },
                _= tasks.next(), if !tasks.is_empty() => {},
                else => return Ok(()),
            };
        }
    }

    async fn serve_subscribe(self, subscribed: Subscribed) -> Result<(), anyhow::Error> {
        let namespace = subscribed.track_namespace.clone();
        let track_name = subscribed.track_name.clone();

// Apply track filter if configured
        if let Some(ref pipeline) = self.filter_pipeline {
            if !pipeline.filter_track(&namespace, &track_name) {
                log::info!(
                    "subscribe rejected by track filter: {}/{}",
                    namespace,
                    track_name
                );
                let err = ServeError::not_found_ctx(format!(
                    "track '{}/{}' rejected by filter policy",
                    namespace, track_name
                ));
                subscribed.close(err.clone())?;
                return Err(err.into());
            }
        }

        if let Some(track_info) = self
            .locals
            .get_or_create_track_info(&namespace, &track_name)
        {
            if track_info.should_subscribe_upstream() {
                log::info!(
                    "subscribe needs upstream request: {}/{}",
                    namespace,
                    track_name
                );

                if let Some(reader) = self.locals.subscribe_upstream(track_info.clone()) {
                    log::info!(
                        "forwarding subscribe upstream via TrackInfo: {}/{}",
                        namespace,
                        track_name
                    );
                    return Ok(subscribed.serve(reader).await?);
                }
            }

            let reader = track_info.get_reader();
            log::info!(
                "serving subscribe from local: {}/{} (state: {:?})",
                namespace,
                track_name,
                track_info.state()
            );
            return Ok(subscribed.serve(reader).await?);
        }

        if let Some(remotes) = self.remotes {
            match remotes.route(&namespace).await {
                Ok(remote) => {
                    if let Some(remote) = remote {
                        if let Some(track) = remote.subscribe(&namespace, &track_name)? {
                            log::info!("serving subscribe from remote: {:?}", track.info);
                            return Ok(subscribed.serve(track.reader).await?);
                        }
                    }
                }
                Err(e) => {
                    log::error!("failed to route to remote: {}", e);
                }
            }
        }

        let err = ServeError::not_found_ctx(format!(
            "track '{}/{}' not found in local or remote tracks",
            namespace, track_name
        ));
        subscribed.close(err.clone())?;
        Err(err.into())
    }

    async fn serve_subscribe_namespace(
        mut self,
        mut subscribe_ns: SubscribeNamespaceReceived,
    ) -> Result<(), anyhow::Error> {
        let namespace_prefix = subscribe_ns.namespace_prefix.clone();
        let track_filter = subscribe_ns.info.track_filter.clone();

        // Create per-subscription TopN filter from TrackFilter if present
        let topn_filter = Self::create_topn_filter_from_track_filter(
            track_filter.as_ref(),
            self.filter_pipeline.as_ref(),
        );

        if let Some(ref filter) = topn_filter {
            let config = filter.config();
            log::info!(
                "SUBSCRIBE_NAMESPACE {:?} has top-n filter: n={}, metric_type=0x{:x}, decay={}ms",
                namespace_prefix,
                config.n,
                config.metric_extension_type,
                config.decay_after.as_millis()
            );
        }

        // Register with subscriber registry to receive PUBLISH and PUBLISH_NAMESPACE notifications
        // If this session is also a publisher, pass their publisher_id for self-exclusion
        let session_publisher_id = self
            .publisher_tracker
            .as_ref()
            .and_then(|t| t.get_publisher_id());

        if session_publisher_id.is_some() {
            log::info!(
                "subscriber is also a publisher (id={}), enabling self-exclusion",
                session_publisher_id.unwrap()
            );
        }

        let (_subscription_guard, mut publish_rx, mut publish_ns_rx) =
            if let Some(ref registry) = self.subscriber_registry {
                let (id, rx, rx_ns) = registry.register_with_publisher_id(
                    namespace_prefix.clone(),
                    track_filter,
                    session_publisher_id,
                );
                (
                    Some(crate::SubscriptionGuard::new(registry.clone(), id)),
                    Some(rx),
                    Some(rx_ns),
                )
            } else {
                (None, None, None)
            };

        // Find existing namespaces that match the prefix
        let matching_namespaces: Vec<TrackNamespace> = self
            .locals
            .matching_namespaces(&namespace_prefix)
            .into_iter()
            .collect();

        // Accept the subscription (even if no current matches - publisher may arrive later)
        subscribe_ns.ok()?;

        log::info!(
            "accepted SUBSCRIBE_NAMESPACE for prefix {:?}, {} existing matches",
            namespace_prefix,
            matching_namespaces.len()
        );

        // Send PUBLISH_NAMESPACE for existing namespaces
        for namespace in matching_namespaces {
            log::info!(
                "sending PUBLISH_NAMESPACE for {:?} (matched prefix {:?})",
                namespace,
                namespace_prefix
            );
            match self.publisher.publish_namespace(namespace.clone()).await {
                Ok(_publish_ns) => {
                    log::debug!("sent PUBLISH_NAMESPACE for {:?}", namespace);
                    // Note: publish_ns is kept alive to maintain the announcement
                }
                Err(e) => {
                    log::warn!(
                        "failed to send PUBLISH_NAMESPACE for {:?}: {}",
                        namespace,
                        e
                    );
                }
            }
        }

        // If we have a publish receiver, listen for new PUBLISH and PUBLISH_NAMESPACE notifications
        if publish_rx.is_some() || publish_ns_rx.is_some() {
            loop {
                tokio::select! {
                    // Wait for the subscription to close
                    result = subscribe_ns.closed() => {
                        result?;
                        break;
                    }
                    // Wait for PUBLISH notifications
                    notification = async {
                        if let Some(ref mut rx) = publish_rx {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        match notification {
                            Ok(publish_notif) => {
                                log::info!(
                                    "received PUBLISH notification for {}/{} on subscription prefix {:?}",
                                    publish_notif.namespace,
                                    publish_notif.track_name,
                                    namespace_prefix
                                );

                                // Get the TrackReader for this track so we can stream data
                                if let Some(track_info) = self.locals.get_track_info(
                                    &publish_notif.namespace,
                                    &publish_notif.track_name,
                                ) {
                                    let track_reader = track_info.get_reader();

                                    // Use publisher.publish() which sends PUBLISH with forward=1
                                    // This allows forwarding objects immediately
                                    let mut publisher = self.publisher.clone();
                                    let ns = publish_notif.namespace.clone();
                                    let name = publish_notif.track_name.clone();
                                    let topn = topn_filter.clone();
                                    let scalable = self.scalable_topn_filter.clone();

                                    // Derive publisher_id from track_alias (in production, use session auth)
                                    let publisher_id = publish_notif.track_alias;

                                    tokio::spawn(async move {
                                        match publisher.publish(track_reader.clone()).await {
                                            Ok(published) => {
                                                log::info!(
                                                    "forwarded PUBLISH for {}/{} with forward=1, streaming immediately",
                                                    ns, name
                                                );

                                                // Serve with appropriate filter
                                                let result = if let Some(ref sf) = scalable {
                                                    // Use scalable filter for global ranking + self-exclusion
                                                    Self::serve_with_scalable_filter(
                                                        published,
                                                        track_reader,
                                                        &ns,
                                                        &name,
                                                        publisher_id,
                                                        sf,
                                                        topn.as_ref(),
                                                    ).await
                                                } else if let Some(ref filter) = topn {
                                                    // Fall back to per-subscription TopN filter
                                                    Self::serve_with_topn_filter(
                                                        published,
                                                        track_reader,
                                                        &ns,
                                                        &name,
                                                        filter,
                                                    ).await
                                                } else {
                                                    // No TopN filter, serve directly
                                                    published.serve_immediately(track_reader).await
                                                };

                                                match result {
                                                    Ok(()) => {
                                                        log::info!("track {}/{} serving completed", ns, name);
                                                    }
                                                    Err(e) => {
                                                        log::warn!(
                                                            "track {}/{} serving ended: {}",
                                                            ns, name, e
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                log::warn!(
                                                    "failed to publish track {}/{}: {}",
                                                    ns, name, e
                                                );
                                            }
                                        }
                                    });
                                } else {
                                    log::warn!(
                                        "no track info found for {}/{}, cannot forward PUBLISH",
                                        publish_notif.namespace,
                                        publish_notif.track_name
                                    );
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                log::warn!("subscription lagged by {} messages", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                log::debug!("publish notification channel closed");
                                break;
                            }
                        }
                    }
                    // Wait for PUBLISH_NAMESPACE notifications -> forward as NAMESPACE message
                    notification = async {
                        if let Some(ref mut rx) = publish_ns_rx {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        match notification {
                            Ok(ns_notif) => {
                                log::info!(
                                    "received PUBLISH_NAMESPACE notification for {:?} on subscription prefix {:?}",
                                    ns_notif.namespace,
                                    namespace_prefix
                                );
                                // Forward NAMESPACE message to the subscriber (not PUBLISH_NAMESPACE)
                                // NAMESPACE (0x08) is the draft-16 message for announcing namespaces
                                // to SUBSCRIBE_NAMESPACE subscribers
                                let namespace_msg = message::Namespace {
                                    id: subscribe_ns.info.request_id,
                                    track_namespace: ns_notif.namespace.clone(),
                                    params: KeyValuePairs::new(),
                                };
                                self.publisher.forward_namespace(namespace_msg);
                                log::debug!(
                                    "forwarded NAMESPACE for {:?} (request_id={})",
                                    ns_notif.namespace,
                                    subscribe_ns.info.request_id
                                );
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                log::warn!("namespace subscription lagged by {} messages", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                log::debug!("publish_namespace notification channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        } else {
            // No registry, just wait for close
            subscribe_ns.closed().await?;
        }

        Ok(())
    }

    /// Serves a track with TopN filtering based on per-object metrics.
    ///
    /// This method:
    /// 1. Creates a filter callback that updates the TopN filter with each object's metric
    /// 2. The callback checks if this track is in top-N and returns whether to forward
    /// 3. Objects are only forwarded when the track is in top-N
    ///
    /// This enables real-time active speaker detection where audio objects carry
    /// audio level metrics in their extension headers.
    async fn serve_with_topn_filter(
        published: moq_transport::session::Published,
        track_reader: moq_transport::serve::TrackReader,
        namespace: &TrackNamespace,
        track_name: &str,
        topn_filter: &Arc<TopNFilter>,
    ) -> Result<(), moq_transport::session::SessionError> {
        let filter = topn_filter.clone();
        let ns = namespace.clone();
        let name = track_name.to_string();
        let metric_type = filter.config().metric_extension_type;

        log::info!(
            "starting filtered serve for {}/{} (top-{}, metric_type=0x{:x})",
            namespace,
            track_name,
            filter.config().n,
            metric_type
        );

        // Create the per-object filter callback
        // This is called for every object and:
        // 1. Extracts the metric value from extension headers
        // 2. Updates the TopN filter with the metric
        // 3. Returns whether this track is in top-N (should forward)
        let object_filter: ObjectFilterFn = Arc::new(move |extensions: &[(u64, u64)]| {
            // Update the TopN filter with the metric from this object
            filter.update_metric(&ns, &name, extensions);

            // Check if this track is currently in top-N
            let should_forward = filter.should_forward(&ns, &name);

            if !should_forward {
                log::trace!(
                    "object filtered: {}/{} not in top-{}",
                    ns,
                    name,
                    filter.config().n
                );
            }

            should_forward
        });

        // Serve with the per-object filter
        published
            .serve_immediately_with_filter(track_reader, object_filter)
            .await
    }

    /// Serves a track with the scalable Top-N filter for global ranking and self-exclusion.
    ///
    /// This method:
    /// 1. Updates the global ScalableTopNFilter with metrics from each object
    /// 2. The filter maintains a global ranking across all tracks
    /// 3. Self-exclusion is handled at notification time (not per-object)
    ///
    /// Unlike serve_with_topn_filter which filters per-subscription, this updates
    /// the global ranking used for self-exclusion in notify_publish.
    async fn serve_with_scalable_filter(
        published: moq_transport::session::Published,
        track_reader: moq_transport::serve::TrackReader,
        namespace: &TrackNamespace,
        track_name: &str,
        publisher_id: PublisherId,
        scalable_filter: &Arc<ScalableTopNFilter>,
        per_sub_filter: Option<&Arc<TopNFilter>>,
    ) -> Result<(), moq_transport::session::SessionError> {
        let global_filter = scalable_filter.clone();
        let per_sub = per_sub_filter.cloned();
        let ns = namespace.clone();
        let ns_str = namespace.to_string();
        let name = track_name.to_string();

        log::info!(
            "starting scalable filtered serve for {}/{} (publisher_id={})",
            namespace,
            track_name,
            publisher_id
        );

        // Create the per-object filter callback
        let object_filter: ObjectFilterFn = Arc::new(move |extensions: &[(u64, u64)]| {
            // Update the global ScalableTopNFilter with metrics
            global_filter.update_metric(publisher_id, &ns_str, &name, extensions);

            // If we have a per-subscription filter, also check that
            if let Some(ref filter) = per_sub {
                filter.update_metric(&ns, &name, extensions);
                let should_forward = filter.should_forward(&ns, &name);
                if !should_forward {
                    log::trace!(
                        "object filtered by per-sub filter: {}/{} not in top-{}",
                        ns,
                        name,
                        filter.config().n
                    );
                }
                return should_forward;
            }

            // Global filter updates ranking but doesn't filter per-object
            // Self-exclusion happens at notify_publish level
            true
        });

        // Serve with the per-object filter
        published
            .serve_immediately_with_filter(track_reader, object_filter)
            .await
    }

    async fn serve_track_status(
        self,
        mut track_status_requested: TrackStatusRequested,
    ) -> Result<(), anyhow::Error> {
        // Check local tracks first, and serve from local if possible
        if let Some(mut local_tracks) = self
            .locals
            .retrieve(&track_status_requested.request_msg.track_namespace)
        {
            if let Some(track) = local_tracks.get_track_reader(
                &track_status_requested.request_msg.track_namespace,
                &track_status_requested.request_msg.track_name,
            ) {
                log::info!("serving track_status from local: {:?}", track.info);
                return Ok(track_status_requested.respond_ok(&track)?);
            }
        }

        // TODO - forward track status to remotes?
        // Check remote tracks second, and serve from remote if possible
        /*
        if let Some(remotes) = &self.remotes {
            // Try to route to a remote for this namespace
            if let Some(remote) = remotes.route(&subscribe.track_namespace).await? {
                if let Some(track) =
                    remote.subscribe(subscribe.track_namespace.clone(), subscribe.track_name.clone())?
                {
                    log::info!("serving from remote: {:?} {:?}", remote.info, track.info);

                    // NOTE: Depends on drop(track) being called afterwards
                    return Ok(subscribe.serve(track.reader).await?);
                }
            }
        }*/

        track_status_requested.respond_error(4, "Track not found")?;

        Err(ServeError::not_found_ctx(format!(
            "track '{}/{}' not found for track_status",
            track_status_requested.request_msg.track_namespace,
            track_status_requested.request_msg.track_name
        ))
        .into())
    }
}
