use std::collections::HashMap;
use std::sync::Arc;

use moq_transport::coding::TrackNamespace;
use moq_transport::message::TrackFilter;
use parking_lot::RwLock;
use tokio::sync::broadcast;

use crate::filter::{PublisherId, ScalableTopNFilter, SubscriberId as FilterSubscriberId};

/// Information about an active SUBSCRIBE_NAMESPACE subscription
#[derive(Clone)]
pub struct NamespaceSubscription {
    /// The namespace prefix this subscription is for
    pub prefix: TrackNamespace,
    /// Optional track filter for top-N selection
    pub track_filter: Option<TrackFilter>,
    /// Channel to send PUBLISH notifications to this subscriber
    pub publish_tx: broadcast::Sender<PublishNotification>,
    /// Channel to send PUBLISH_NAMESPACE notifications to this subscriber
    pub publish_ns_tx: broadcast::Sender<PublishNamespaceNotification>,
    /// If this subscriber is also a publisher, their publisher ID (for self-exclusion)
    pub publisher_id: Option<PublisherId>,
    /// Filter subscriber ID (for scalable filter integration)
    pub filter_subscriber_id: Option<FilterSubscriberId>,
}

/// Notification sent when a PUBLISH arrives that matches a subscription
#[derive(Clone, Debug)]
pub struct PublishNotification {
    pub namespace: TrackNamespace,
    pub track_name: String,
    pub track_alias: u64,
}

/// Notification sent when a PUBLISH_NAMESPACE arrives that matches a subscription
#[derive(Clone, Debug)]
pub struct PublishNamespaceNotification {
    pub namespace: TrackNamespace,
}

/// Registry for tracking active SUBSCRIBE_NAMESPACE subscriptions
///
/// When a subscriber sends SUBSCRIBE_NAMESPACE, they register here.
/// When a publisher sends PUBLISH, we find matching subscriptions and notify.
///
/// Uses RwLock for better read-heavy performance at scale.
#[derive(Clone)]
pub struct SubscriberRegistry {
    inner: Arc<RwLock<SubscriberRegistryInner>>,
    /// Optional scalable Top-N filter for self-exclusion support
    topn_filter: Option<Arc<ScalableTopNFilter>>,
}

struct SubscriberRegistryInner {
    /// Map from subscription ID to subscription info
    subscriptions: HashMap<u64, NamespaceSubscription>,
    /// Next subscription ID
    next_id: u64,
}

impl SubscriberRegistry {
    /// Create a new registry without Top-N filtering
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SubscriberRegistryInner {
                subscriptions: HashMap::new(),
                next_id: 0,
            })),
            topn_filter: None,
        }
    }

    /// Create a new registry with Top-N filtering and self-exclusion support
    pub fn with_topn_filter(filter: Arc<ScalableTopNFilter>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(SubscriberRegistryInner {
                subscriptions: HashMap::new(),
                next_id: 0,
            })),
            topn_filter: Some(filter),
        }
    }

    /// Get the Top-N filter if configured
    pub fn topn_filter(&self) -> Option<&Arc<ScalableTopNFilter>> {
        self.topn_filter.as_ref()
    }

    /// Register a SUBSCRIBE_NAMESPACE subscription (viewer - no self-exclusion)
    /// Returns (subscription_id, receiver for PUBLISH notifications, receiver for PUBLISH_NAMESPACE notifications)
    pub fn register(
        &self,
        prefix: TrackNamespace,
        track_filter: Option<TrackFilter>,
    ) -> (
        u64,
        broadcast::Receiver<PublishNotification>,
        broadcast::Receiver<PublishNamespaceNotification>,
    ) {
        self.register_with_publisher_id(prefix, track_filter, None)
    }

    /// Register a SUBSCRIBE_NAMESPACE subscription with optional publisher ID for self-exclusion
    ///
    /// If publisher_id is Some, the subscriber won't receive their own tracks in Top-N selection.
    pub fn register_with_publisher_id(
        &self,
        prefix: TrackNamespace,
        track_filter: Option<TrackFilter>,
        publisher_id: Option<PublisherId>,
    ) -> (
        u64,
        broadcast::Receiver<PublishNotification>,
        broadcast::Receiver<PublishNamespaceNotification>,
    ) {
        let mut inner = self.inner.write();

        let id = inner.next_id;
        inner.next_id += 1;

        // Create broadcast channels for PUBLISH and PUBLISH_NAMESPACE notifications
        let (publish_tx, publish_rx) = broadcast::channel(64);
        let (publish_ns_tx, publish_ns_rx) = broadcast::channel(64);

        // Register with the scalable filter if configured
        let filter_subscriber_id = if track_filter.is_some() {
            self.topn_filter.as_ref().map(|filter| {
                if let Some(pub_id) = publisher_id {
                    // Publisher-subscriber: needs self-exclusion
                    let (filter_id, _rx) = filter.register_publisher_subscriber(pub_id);
                    filter_id
                } else {
                    // Pure viewer: shared broadcast
                    let (filter_id, _rx) = filter.register_viewer();
                    filter_id
                }
            })
        } else {
            None
        };

        let subscription = NamespaceSubscription {
            prefix,
            track_filter,
            publish_tx,
            publish_ns_tx,
            publisher_id,
            filter_subscriber_id,
        };

        inner.subscriptions.insert(id, subscription);

        log::debug!(
            "registered namespace subscription id={} publisher_id={:?}",
            id,
            publisher_id
        );

        (id, publish_rx, publish_ns_rx)
    }

    /// Unregister a subscription
    pub fn unregister(&self, id: u64) {
        let mut inner = self.inner.write();
        if let Some(sub) = inner.subscriptions.remove(&id) {
            // Unregister from the scalable filter if applicable
            if let (Some(filter), Some(filter_id)) = (&self.topn_filter, sub.filter_subscriber_id) {
                filter.unregister(filter_id);
            }
            log::debug!("unregistered namespace subscription id={}", id);
        }
    }

    /// Find all subscriptions that match a given namespace and notify them of a PUBLISH
    /// Returns the number of matching subscriptions notified
    pub fn notify_publish(
        &self,
        namespace: &TrackNamespace,
        track_name: &str,
        track_alias: u64,
    ) -> usize {
        self.notify_publish_with_filter(namespace, track_name, track_alias, None)
    }

    /// Find all subscriptions that match a given namespace and notify them of a PUBLISH
    /// with optional publisher_id for self-exclusion filtering.
    ///
    /// If a ScalableTopNFilter is configured and the subscription has a track_filter,
    /// this will apply the filter and self-exclusion rules.
    ///
    /// Returns the number of matching subscriptions notified
    pub fn notify_publish_with_filter(
        &self,
        namespace: &TrackNamespace,
        track_name: &str,
        track_alias: u64,
        _track_publisher_id: Option<PublisherId>,
    ) -> usize {
        let inner = self.inner.read();

        let notification = PublishNotification {
            namespace: namespace.clone(),
            track_name: track_name.to_string(),
            track_alias,
        };

        let namespace_str = namespace.to_string();
        let mut notified = 0;

        for (id, sub) in inner.subscriptions.iter() {
            // Check if the namespace matches the subscription prefix
            // The subscription prefix should be a prefix of the namespace
            if Self::prefix_matches(&sub.prefix, namespace) {
                // Apply track_filter if present and filter is configured
                let should_forward = if sub.track_filter.is_some() {
                    if let (Some(filter), Some(filter_sub_id)) =
                        (&self.topn_filter, sub.filter_subscriber_id)
                    {
                        // Use the scalable filter with self-exclusion
                        filter.should_forward(&namespace_str, track_name, filter_sub_id)
                    } else {
                        // No filter configured, forward all
                        true
                    }
                } else {
                    // No track_filter on subscription, forward all
                    true
                };

                if !should_forward {
                    log::trace!(
                        "filtered out PUBLISH {}/{} for subscription id={}",
                        namespace,
                        track_name,
                        id
                    );
                    continue;
                }

                if let Err(e) = sub.publish_tx.send(notification.clone()) {
                    log::warn!("failed to notify subscription id={}: {}", id, e);
                } else {
                    log::debug!(
                        "notified subscription id={} of PUBLISH {}/{}",
                        id,
                        namespace,
                        track_name
                    );
                    notified += 1;
                }
            }
        }

        notified
    }

    /// Update a track's metric in the filter (call when receiving metric data)
    ///
    /// This should be called when objects with metric extension headers arrive.
    /// Uses TrackNamespace for consistent key formatting.
    pub fn update_track_metric(
        &self,
        publisher_id: PublisherId,
        namespace: &TrackNamespace,
        track_name: &str,
        extensions: &[(u64, u64)],
    ) -> Option<u64> {
        let namespace_str = namespace.to_string();
        self.topn_filter
            .as_ref()
            .and_then(|f| f.update_metric(publisher_id, &namespace_str, track_name, extensions))
    }

    /// Force processing of pending metric updates
    pub fn process_filter_batch(&self) {
        if let Some(filter) = &self.topn_filter {
            filter.process_batch();
        }
    }

    /// Find all subscriptions that match a given namespace and notify them of a PUBLISH_NAMESPACE
    /// Returns the number of matching subscriptions notified
    pub fn notify_publish_namespace(&self, namespace: &TrackNamespace) -> usize {
        let inner = self.inner.read();

        let notification = PublishNamespaceNotification {
            namespace: namespace.clone(),
        };

        let mut notified = 0;

        for (id, sub) in inner.subscriptions.iter() {
            // Check if the namespace matches the subscription prefix
            if Self::prefix_matches(&sub.prefix, namespace) {
                if let Err(e) = sub.publish_ns_tx.send(notification.clone()) {
                    log::warn!(
                        "failed to notify subscription id={} of PUBLISH_NAMESPACE: {}",
                        id,
                        e
                    );
                } else {
                    log::debug!(
                        "notified subscription id={} of PUBLISH_NAMESPACE {:?}",
                        id,
                        namespace
                    );
                    notified += 1;
                }
            }
        }

        notified
    }

    /// Check if prefix is a prefix of namespace
    fn prefix_matches(prefix: &TrackNamespace, namespace: &TrackNamespace) -> bool {
        if prefix.fields.len() > namespace.fields.len() {
            return false;
        }

        prefix
            .fields
            .iter()
            .zip(namespace.fields.iter())
            .all(|(a, b)| a == b)
    }

    /// Get all subscriptions matching a prefix (for debugging)
    pub fn matching_subscriptions(&self, namespace: &TrackNamespace) -> Vec<u64> {
        let inner = self.inner.read();

        inner
            .subscriptions
            .iter()
            .filter(|(_, sub)| Self::prefix_matches(&sub.prefix, namespace))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Get statistics about the registry
    pub fn stats(&self) -> RegistryStats {
        let inner = self.inner.read();
        let filter_stats = self.topn_filter.as_ref().map(|f| f.stats());

        RegistryStats {
            subscription_count: inner.subscriptions.len(),
            filter_stats,
        }
    }
}

/// Statistics about the subscriber registry
#[derive(Debug, Clone)]
pub struct RegistryStats {
    pub subscription_count: usize,
    pub filter_stats: Option<crate::filter::ScalableTopNStats>,
}

impl Default for SubscriberRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard that unregisters on drop
pub struct SubscriptionGuard {
    registry: SubscriberRegistry,
    id: u64,
}

impl SubscriptionGuard {
    pub fn new(registry: SubscriberRegistry, id: u64) -> Self {
        Self { registry, id }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.registry.unregister(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{ScalableTopNConfig, ScalableTopNFilter};

    fn ns(path: &str) -> TrackNamespace {
        TrackNamespace::from_utf8_path(path)
    }

    #[test]
    fn test_prefix_matching() {
        assert!(SubscriberRegistry::prefix_matches(&ns("live"), &ns("live/stream1")));
        assert!(SubscriberRegistry::prefix_matches(&ns("live"), &ns("live")));
        // An empty prefix (zero fields) should match everything
        let empty = TrackNamespace::new();
        assert!(SubscriberRegistry::prefix_matches(&empty, &ns("live/stream1")));
        assert!(!SubscriberRegistry::prefix_matches(&ns("live/stream1"), &ns("live")));
        assert!(!SubscriberRegistry::prefix_matches(&ns("other"), &ns("live/stream1")));
    }

    #[test]
    fn test_register_unregister() {
        let registry = SubscriberRegistry::new();

        let (id1, _rx1, _rx1_ns) = registry.register(ns("live"), None);
        let (id2, _rx2, _rx2_ns) = registry.register(ns("live/room1"), None);

        assert_eq!(registry.matching_subscriptions(&ns("live/room1/track")).len(), 2);

        registry.unregister(id1);

        assert_eq!(registry.matching_subscriptions(&ns("live/room1/track")).len(), 1);

        registry.unregister(id2);

        assert_eq!(registry.matching_subscriptions(&ns("live/room1/track")).len(), 0);
    }

    #[tokio::test]
    async fn test_notify_publish() {
        let registry = SubscriberRegistry::new();

        let (id, mut rx, _rx_ns) = registry.register(ns("live"), None);

        let notified = registry.notify_publish(&ns("live/stream1"), "video", 100);
        assert_eq!(notified, 1);

        let notification = rx.recv().await.unwrap();
        assert_eq!(notification.track_name, "video");
        assert_eq!(notification.track_alias, 100);

        registry.unregister(id);
    }

    #[tokio::test]
    async fn test_self_exclusion_with_filter() {
        // Create filter for top-2 tracks
        let filter = Arc::new(ScalableTopNFilter::new(ScalableTopNConfig::top(2)));
        let registry = SubscriberRegistry::with_topn_filter(filter.clone());

        let room_ns = ns("room");

        // Publishers send tracks with metrics
        // Alice (publisher 1) has highest metric
        registry.update_track_metric(1, &room_ns, "alice_audio", &[(0x100, 200)]);
        // Bob (publisher 2) has second highest
        registry.update_track_metric(2, &room_ns, "bob_audio", &[(0x100, 150)]);
        // Charlie (publisher 3) has lowest
        registry.update_track_metric(3, &room_ns, "charlie_audio", &[(0x100, 100)]);

        // Process the batch
        registry.process_filter_batch();

        // Create a track filter (active speaker with top 2)
        let track_filter = Some(moq_transport::message::TrackFilter::active_speaker(2, 2000));

        // Register Alice as a publisher-subscriber (she's also a publisher)
        let (alice_id, mut alice_rx, _) =
            registry.register_with_publisher_id(room_ns.clone(), track_filter.clone(), Some(1));

        // Register a pure viewer (no publisher_id)
        let (viewer_id, mut viewer_rx, _) = registry.register(room_ns.clone(), track_filter.clone());

        // Notify about Alice's track
        let alice_notified = registry.notify_publish(&room_ns, "alice_audio", 1);
        // Viewer should get notified (alice is in top-2)
        // Alice should NOT get notified (self-exclusion)
        assert_eq!(alice_notified, 1); // Only viewer

        // Notify about Bob's track
        let bob_notified = registry.notify_publish(&room_ns, "bob_audio", 2);
        // Both should get notified
        assert_eq!(bob_notified, 2);

        // Notify about Charlie's track
        let charlie_notified = registry.notify_publish(&room_ns, "charlie_audio", 3);
        // Viewer shouldn't get it (charlie not in top-2)
        // Alice SHOULD get it (charlie is in HER top-2 after self-exclusion)
        assert_eq!(charlie_notified, 1); // Only Alice

        // Verify viewer received alice and bob
        let v1 = viewer_rx.recv().await.unwrap();
        assert_eq!(v1.track_name, "alice_audio");
        let v2 = viewer_rx.recv().await.unwrap();
        assert_eq!(v2.track_name, "bob_audio");

        // Verify Alice received bob and charlie (not herself)
        let a1 = alice_rx.recv().await.unwrap();
        assert_eq!(a1.track_name, "bob_audio");
        let a2 = alice_rx.recv().await.unwrap();
        assert_eq!(a2.track_name, "charlie_audio");

        registry.unregister(alice_id);
        registry.unregister(viewer_id);
    }
}
