use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use moq_transport::{
    message::DtsParams,
    serve::{ServeError, TracksReader},
    session::{
        PublishNamespace, Publisher, SessionError, SubscribeNamespaceReceived, Subscribed,
        TrackStatusRequested,
    },
};

use crate::{DtsServiceHandle, Locals, RemotesConsumer, SubscriberRegistry};

/// Producer of tracks to a remote Subscriber
#[derive(Clone)]
pub struct Producer {
    publisher: Publisher,
    locals: Locals,
    remotes: Option<RemotesConsumer>,
    subscriber_registry: Option<SubscriberRegistry>,
    session_id: u64,
    dts_service: Option<DtsServiceHandle>,
}

impl Producer {
    pub fn new(publisher: Publisher, locals: Locals, remotes: Option<RemotesConsumer>) -> Self {
        Self {
            publisher,
            locals,
            remotes,
            subscriber_registry: None,
            session_id: 0,
            dts_service: None,
        }
    }

    /// Creates a producer with a subscriber registry.
    pub fn with_registry(
        publisher: Publisher,
        locals: Locals,
        remotes: Option<RemotesConsumer>,
        subscriber_registry: SubscriberRegistry,
        session_id: u64,
    ) -> Self {
        Self {
            publisher,
            locals,
            remotes,
            subscriber_registry: Some(subscriber_registry),
            session_id,
            dts_service: None,
        }
    }

    /// Creates a producer with a subscriber registry and DTS service.
    pub fn with_registry_and_dts(
        publisher: Publisher,
        locals: Locals,
        remotes: Option<RemotesConsumer>,
        subscriber_registry: SubscriberRegistry,
        session_id: u64,
        dts_service: Option<DtsServiceHandle>,
    ) -> Self {
        Self {
            publisher,
            locals,
            remotes,
            subscriber_registry: Some(subscriber_registry),
            session_id,
            dts_service,
        }
    }

    pub async fn publish_namespace(
        &mut self,
        tracks: TracksReader,
    ) -> Result<PublishNamespace, SessionError> {
        self.publisher
            .publish_namespace(tracks.namespace.clone())
            .await
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
                    let dts_service = self.dts_service.clone();
                    let session_id = self.session_id;

                    // Spawn a new task to handle the subscribe
                    let locals = this.locals.clone();
                    tasks.push(async move {
                        let info = subscribed.clone();
                        let subscription_id = info.id;
                        let namespace = info.track_namespace.clone();
                        let track_name = info.track_name.clone();
                        log::info!("serving subscribe: {:?}", info);

                        // Serve the subscribe request
                        let result = this.serve_subscribe(subscribed).await;

                        // When serve_subscribe returns (success or error), remove from DTS
                        // This triggers reselection if this was the selected track
                        log::info!(
                            "DTS_DEBUG: serve_subscribe returned for subscription_id={} track={}/{}",
                            subscription_id, namespace, track_name
                        );
                        if let Some(ref dts) = dts_service {
                            let change = dts.remove_track(session_id, subscription_id);
                            if !change.newly_selected.is_empty() {
                                log::info!(
                                    "DTS: subscription {} ended, new selection: {:?}",
                                    subscription_id,
                                    change.newly_selected.iter().map(|s| &s.track.track_name).collect::<Vec<_>>()
                                );
                            }
                        }

                        // Reset the upstream subscribe flag so future subscriptions can trigger
                        // a new upstream subscribe. This is needed because when the downstream
                        // subscription ends (e.g., subscriber unsubscribes), the upstream reader
                        // may be closed, and we need to allow re-subscribing.
                        if let Some(track_info) = locals.get_track_info(&namespace, &track_name) {
                            track_info.reset_upstream_subscribe();
                            log::info!(
                                "subscription {} ended, reset upstream subscribe flag for {}/{}",
                                subscription_id, namespace, track_name
                            );
                        }

                        if let Err(err) = result {
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
        let subscription_id = subscribed.info.id;

        // Check for DTS SWITCHING-SET-ASSIGNMENT parameter and register with DTS service
        let is_dts_track = if let Some(ref dts) = self.dts_service {
            if subscribed.info.params.has_switching_set_assignment() {
                let registered = dts.register_track_from_subscribe(
                    self.session_id,
                    subscription_id,
                    &namespace,
                    &track_name,
                    &subscribed.info.params,
                );
                if registered {
                    log::info!(
                        "registered DTS track {}/{} for session {} (subscription_id={})",
                        namespace, track_name, self.session_id, subscription_id
                    );
                }

                // Wait for the switching set to be activated (max 500ms)
                let mut wait_count = 0;
                while !dts.is_set_active_for_track(self.session_id, subscription_id) && wait_count < 50 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    wait_count += 1;
                }

                if !dts.is_set_active_for_track(self.session_id, subscription_id) {
                    log::warn!(
                        "DTS: track {}/{} switching set never activated, serving empty",
                        namespace, track_name
                    );
                    return Ok(subscribed.serve_empty().await?);
                }

                // Wait for bandwidth to be measured (max 1000ms)
                let mut bw_wait_count = 0;
                while dts.get_bandwidth_kbps(self.session_id) == 0 && bw_wait_count < 100 {
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    bw_wait_count += 1;
                }

                let bandwidth = dts.get_bandwidth_kbps(self.session_id);
                if bandwidth == 0 {
                    log::warn!(
                        "DTS: no bandwidth measurement for session {}, using lowest quality",
                        self.session_id
                    );
                    dts.set_bandwidth(self.session_id, 800);
                } else {
                    log::info!(
                        "DTS: bandwidth measured {} kbps for session {}",
                        bandwidth, self.session_id
                    );
                }

                true // This is a DTS track
            } else {
                false
            }
        } else {
            false
        };

        // Build DTS group filter and bytes callback if this is a DTS track
        // The filter checks should_forward_object on each group boundary
        // The bytes callback tracks actual throughput for bandwidth estimation
        let (dts_filter, bytes_callback) = if is_dts_track {
            if let Some(ref dts) = self.dts_service {
                let dts_clone = dts.clone();
                let dts_clone2 = dts.clone();
                let session_id = self.session_id;
                let filter = Some(Box::new(move |group_id: u64, object_id: u64| -> bool {
                    let should_forward = dts_clone.should_forward_object(session_id, subscription_id, group_id, object_id);
                    if !should_forward {
                        log::debug!(
                            "DTS: filtering group {} for subscription {} (not selected)",
                            group_id, subscription_id
                        );
                    }
                    should_forward
                }) as moq_transport::session::GroupFilterFn);
                let callback = Some(Box::new(move |bytes: u64| {
                    dts_clone2.record_bytes_sent(session_id, bytes);
                }) as moq_transport::session::BytesSentCallback);
                (filter, callback)
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

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
                    // Use DTS filter and bytes callback if present
                    return if dts_filter.is_some() || bytes_callback.is_some() {
                        Ok(subscribed.serve_with_filter_and_bytes_callback(reader, dts_filter, bytes_callback.unwrap_or_else(|| Box::new(|_| {}))).await?)
                    } else {
                        Ok(subscribed.serve(reader).await?)
                    };
                }
            }

            // If the track is in Publishing state and forward=0, request forwarding
            // This will trigger the consumer to send REQUEST_UPDATE to the publisher
            if track_info.is_publishing() && !track_info.is_forwarding() {
                log::info!(
                    "subscriber arrived for paused track {}/{}, requesting forward",
                    namespace,
                    track_name
                );
                track_info.request_forward();
            }

            let reader = track_info.get_reader();
            log::info!(
                "serving subscribe from local: {}/{} (state: {:?}, forwarding: {})",
                namespace,
                track_name,
                track_info.state(),
                track_info.is_forwarding()
            );

            // Use DTS filter and bytes callback if present
            return if dts_filter.is_some() || bytes_callback.is_some() {
                Ok(subscribed.serve_with_filter_and_bytes_callback(reader, dts_filter, bytes_callback.unwrap_or_else(|| Box::new(|_| {}))).await?)
            } else {
                Ok(subscribed.serve(reader).await?)
            };
        }

        if let Some(remotes) = self.remotes {
            match remotes.route(&namespace).await {
                Ok(remote) => {
                    if let Some(remote) = remote {
                        if let Some(track) = remote.subscribe(&namespace, &track_name)? {
                            log::info!("serving subscribe from remote: {:?}", track.info);
                            // Use DTS filter and bytes callback if present
                            return if dts_filter.is_some() || bytes_callback.is_some() {
                                Ok(subscribed.serve_with_filter_and_bytes_callback(track.reader, dts_filter, bytes_callback.unwrap_or_else(|| Box::new(|_| {}))).await?)
                            } else {
                                Ok(subscribed.serve(track.reader).await?)
                            };
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

        // Parse TRACK_FILTER from params if present
        // TRACK_FILTER key is 0x12 (even = int value)
        // Value format: (property_type << 8) | max_selected packed into u64
        const TRACK_FILTER_KEY: u64 = 0x12;
        let track_filter = subscribe_ns.info.params.get(TRACK_FILTER_KEY).and_then(|kvp| {
            if let moq_transport::coding::Value::IntValue(packed) = &kvp.value {
                // Unpack: property_type in high byte, max_selected in low byte
                let property_type = (*packed >> 8) & 0xFF;
                let max_selected = (*packed & 0xFF) as u8;
                log::info!(
                    "parsed TRACK_FILTER: property_type={}, max_selected={}",
                    property_type,
                    max_selected
                );
                Some(crate::TrackFilter {
                    property_type,
                    max_selected,
                })
            } else {
                None
            }
        });

        // Register with subscriber registry to receive PUBLISH and PUBLISH_NAMESPACE notifications
        // Uses session_id so we can exclude PUBLISH messages from the same session (self-exclusion)
        let (_subscription_guard, mut publish_rx, mut publish_ns_rx) =
            if let Some(ref registry) = self.subscriber_registry {
                let (id, rx, rx_ns) = registry.register_with_filter(
                    namespace_prefix.clone(),
                    self.session_id,
                    track_filter,
                );
                (
                    Some(crate::SubscriptionGuard::new(registry.clone(), id)),
                    Some(rx),
                    Some(rx_ns),
                )
            } else {
                (None, None, None)
            };

        // Accept the subscription (even if no current matches - publisher may arrive later)
        subscribe_ns.ok()?;

        log::info!(
            "accepted SUBSCRIBE_NAMESPACE for prefix {:?}",
            namespace_prefix
        );

        // Send PUBLISH for existing tracks in matching namespaces
        // This triggers the client's onMatch callback for track discovery
        // Note: We skip PUBLISH_NAMESPACE and send PUBLISH directly - client expects PUBLISH for tracks
        let matching_tracks = self.locals.matching_tracks(&namespace_prefix);
        log::info!(
            "found {} existing tracks matching prefix {:?}",
            matching_tracks.len(),
            namespace_prefix
        );

        for (ns, track_name, track_info) in matching_tracks {
            let track_extensions = track_info.track_extensions().unwrap_or_default();
            log::info!(
                "sending PUBLISH for existing track {}/{} (matched prefix {:?}, extensions={:?})",
                ns,
                track_name,
                namespace_prefix,
                track_extensions
            );

            let track_reader = track_info.get_reader();
            let mut publisher = self.publisher.clone();
            let registry = self.subscriber_registry.clone();
            let session_id = self.session_id;
            // For existing tracks, we use track_alias 0 as placeholder (not used in TopN update notifications)
            let track_alias = 0u64;

            tokio::spawn(async move {
                match publisher.publish_with_extensions(track_reader.clone(), track_extensions).await {
                    Ok(published) => {
                        log::info!(
                            "sent PUBLISH for existing track {}/{}, waiting for PUBLISH_OK",
                            ns,
                            track_name
                        );
                        // Create observer to update TopN tracker and filter objects
                        let observer = if let Some(ref reg) = registry {
                            let reg = reg.clone();
                            let ns_for_observer = ns.clone();
                            let name_for_observer = track_name.clone();
                            let track_filter = reg.get_track_filter_for_session(session_id);
                            Some(moq_transport::session::ObjectObserverFn::from(
                                Box::new(move |group_id: u64, object_id: u64, ext_headers: &moq_transport::data::ExtensionHeaders| {
                                    const AUDIO_LEVEL_EXT: u64 = 0x12;
                                    if let Some(kvp) = ext_headers.get(AUDIO_LEVEL_EXT) {
                                        if let moq_transport::coding::Value::IntValue(value) = kvp.value {
                                            log::debug!(
                                                "object observer (existing): {}/{} group={} obj={} audio_level={}",
                                                ns_for_observer, name_for_observer, group_id, object_id, value
                                            );
                                            reg.update_track_value(
                                                &ns_for_observer,
                                                &name_for_observer,
                                                AUDIO_LEVEL_EXT,
                                                value,
                                                track_alias,
                                                session_id,
                                            );
                                        }
                                    }
                                    // Check if this track should be forwarded to this subscriber
                                    if let Some(ref filter) = track_filter {
                                        let in_top_n = reg.is_track_in_top_n(
                                            &ns_for_observer,
                                            &name_for_observer,
                                            session_id,
                                            filter.property_type,
                                            filter.max_selected,
                                        );
                                        log::debug!(
                                            "object filter (existing): {}/{} group={} obj={} in_top_{}={} for session {}",
                                            ns_for_observer, name_for_observer, group_id, object_id,
                                            filter.max_selected, in_top_n, session_id
                                        );
                                        in_top_n
                                    } else {
                                        true
                                    }
                                }) as Box<dyn Fn(u64, u64, &moq_transport::data::ExtensionHeaders) -> bool + Send + Sync>
                            ))
                        } else {
                            None
                        };

                        let result = if let Some(obs) = observer {
                            published.serve_with_observer(track_reader, obs).await
                        } else {
                            published.serve(track_reader).await
                        };

                        match result {
                            Ok(()) => {
                                log::info!("existing track {}/{} serving completed", ns, track_name);
                            }
                            Err(e) => {
                                log::warn!("existing track {}/{} serving ended: {}", ns, track_name, e);
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("failed to send PUBLISH for existing track {}/{}: {}", ns, track_name, e);
                    }
                }
            });
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
                    // Wait for PUBLISH notifications -> forward PUBLISH to subscriber
                    // Subscriber sends PUBLISH_OK, then relay starts streaming data
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
                                    let track_extensions = track_info.track_extensions().unwrap_or_default();

                                    // Send PUBLISH and wait for PUBLISH_OK before streaming
                                    let mut publisher = self.publisher.clone();
                                    let ns = publish_notif.namespace.clone();
                                    let name = publish_notif.track_name.clone();
                                    let registry = self.subscriber_registry.clone();
                                    let track_alias = publish_notif.track_alias;
                                    let session_id = self.session_id;
                                    log::info!(
                                        "forwarding PUBLISH for {}/{} with extensions {:?}",
                                        ns, name, track_extensions
                                    );
                                    tokio::spawn(async move {
                                        match publisher.publish_with_extensions(track_reader.clone(), track_extensions).await {
                                            Ok(published) => {
                                                log::info!(
                                                    "sent PUBLISH for {}/{}, waiting for PUBLISH_OK",
                                                    ns, name
                                                );
                                                // Create observer to update TopN tracker and filter objects
                                                // The observer both updates the tracker AND returns whether to forward
                                                let observer = if let Some(ref reg) = registry {
                                                    let reg = reg.clone();
                                                    let ns_for_observer = ns.clone();
                                                    let name_for_observer = name.clone();
                                                    // Get subscriber's track filter to check top-N membership
                                                    let track_filter = reg.get_track_filter_for_session(session_id);
                                                    Some(moq_transport::session::ObjectObserverFn::from(
                                                        Box::new(move |group_id: u64, object_id: u64, ext_headers: &moq_transport::data::ExtensionHeaders| {
                                                            // Extract audio level property (0x12) from extension headers
                                                            const AUDIO_LEVEL_EXT: u64 = 0x12;
                                                            if let Some(kvp) = ext_headers.get(AUDIO_LEVEL_EXT) {
                                                                if let moq_transport::coding::Value::IntValue(value) = kvp.value {
                                                                    log::debug!(
                                                                        "object observer: {}/{} group={} obj={} audio_level={}",
                                                                        ns_for_observer, name_for_observer, group_id, object_id, value
                                                                    );
                                                                    // Update tracker with new value (notifies other subscribers if track enters their top-N)
                                                                    reg.update_track_value(
                                                                        &ns_for_observer,
                                                                        &name_for_observer,
                                                                        AUDIO_LEVEL_EXT,
                                                                        value,
                                                                        track_alias,
                                                                        session_id,
                                                                    );
                                                                }
                                                            }

                                                            // Check if this track should be forwarded to this subscriber
                                                            // If subscriber has no track_filter, always forward
                                                            if let Some(ref filter) = track_filter {
                                                                let in_top_n = reg.is_track_in_top_n(
                                                                    &ns_for_observer,
                                                                    &name_for_observer,
                                                                    session_id,
                                                                    filter.property_type,
                                                                    filter.max_selected,
                                                                );
                                                                log::debug!(
                                                                    "object filter: {}/{} group={} obj={} in_top_{}={} for session {}",
                                                                    ns_for_observer, name_for_observer, group_id, object_id,
                                                                    filter.max_selected, in_top_n, session_id
                                                                );
                                                                in_top_n
                                                            } else {
                                                                // No filter = always forward
                                                                true
                                                            }
                                                        }) as Box<dyn Fn(u64, u64, &moq_transport::data::ExtensionHeaders) -> bool + Send + Sync>
                                                    ))
                                                } else {
                                                    None
                                                };

                                                // serve with observer to update TopN from object extension headers
                                                let result = if let Some(obs) = observer {
                                                    published.serve_with_observer(track_reader, obs).await
                                                } else {
                                                    published.serve(track_reader).await
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
                                                    "failed to send PUBLISH for {}/{}: {}",
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
                    // PUBLISH_NAMESPACE notifications - we don't forward these as NAMESPACE messages
                    // Client expects PUBLISH for individual tracks, not namespace announcements
                    notification = async {
                        if let Some(ref mut rx) = publish_ns_rx {
                            rx.recv().await
                        } else {
                            std::future::pending().await
                        }
                    } => {
                        match notification {
                            Ok(ns_notif) => {
                                log::debug!(
                                    "ignoring PUBLISH_NAMESPACE notification for {:?} (client expects PUBLISH for tracks)",
                                    ns_notif.namespace
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
