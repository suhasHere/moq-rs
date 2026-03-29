use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use moq_transport::coding::TrackNamespace;
use moq_transport::message::TrackFilter;
use tokio::sync::broadcast;

/// Information about an active SUBSCRIBE_NAMESPACE subscription
#[derive(Clone)]
pub struct NamespaceSubscription {
    /// The namespace prefix this subscription is for
    pub prefix: TrackNamespace,
    /// Optional track filter for top-N selection
    pub track_filter: Option<TrackFilter>,
    /// Channel to send PUBLISH notifications to this subscriber
    pub publish_tx: broadcast::Sender<PublishNotification>,
}

/// Notification sent when a PUBLISH arrives that matches a subscription
#[derive(Clone, Debug)]
pub struct PublishNotification {
    pub namespace: TrackNamespace,
    pub track_name: String,
    pub track_alias: u64,
}

/// Registry for tracking active SUBSCRIBE_NAMESPACE subscriptions
///
/// When a subscriber sends SUBSCRIBE_NAMESPACE, they register here.
/// When a publisher sends PUBLISH, we find matching subscriptions and notify.
#[derive(Clone)]
pub struct SubscriberRegistry {
    inner: Arc<Mutex<SubscriberRegistryInner>>,
}

struct SubscriberRegistryInner {
    /// Map from subscription ID to subscription info
    subscriptions: HashMap<u64, NamespaceSubscription>,
    /// Next subscription ID
    next_id: u64,
}

impl SubscriberRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SubscriberRegistryInner {
                subscriptions: HashMap::new(),
                next_id: 0,
            })),
        }
    }

    /// Register a SUBSCRIBE_NAMESPACE subscription
    /// Returns (subscription_id, receiver for PUBLISH notifications)
    pub fn register(
        &self,
        prefix: TrackNamespace,
        track_filter: Option<TrackFilter>,
    ) -> (u64, broadcast::Receiver<PublishNotification>) {
        let mut inner = self.inner.lock().unwrap();

        let id = inner.next_id;
        inner.next_id += 1;

        // Create a broadcast channel for PUBLISH notifications
        let (tx, rx) = broadcast::channel(64);

        let subscription = NamespaceSubscription {
            prefix,
            track_filter,
            publish_tx: tx,
        };

        inner.subscriptions.insert(id, subscription);

        log::debug!("registered namespace subscription id={}", id);

        (id, rx)
    }

    /// Unregister a subscription
    pub fn unregister(&self, id: u64) {
        let mut inner = self.inner.lock().unwrap();
        if inner.subscriptions.remove(&id).is_some() {
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
        let inner = self.inner.lock().unwrap();

        let notification = PublishNotification {
            namespace: namespace.clone(),
            track_name: track_name.to_string(),
            track_alias,
        };

        let mut notified = 0;

        for (id, sub) in inner.subscriptions.iter() {
            // Check if the namespace matches the subscription prefix
            // The subscription prefix should be a prefix of the namespace
            if Self::prefix_matches(&sub.prefix, namespace) {
                // TODO: Apply track_filter here if present
                // For now, just notify all matching subscriptions

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
        let inner = self.inner.lock().unwrap();

        inner
            .subscriptions
            .iter()
            .filter(|(_, sub)| Self::prefix_matches(&sub.prefix, namespace))
            .map(|(id, _)| *id)
            .collect()
    }
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

        let (id1, _rx1) = registry.register(ns("live"), None);
        let (id2, _rx2) = registry.register(ns("live/room1"), None);

        assert_eq!(registry.matching_subscriptions(&ns("live/room1/track")).len(), 2);

        registry.unregister(id1);

        assert_eq!(registry.matching_subscriptions(&ns("live/room1/track")).len(), 1);

        registry.unregister(id2);

        assert_eq!(registry.matching_subscriptions(&ns("live/room1/track")).len(), 0);
    }

    #[tokio::test]
    async fn test_notify_publish() {
        let registry = SubscriberRegistry::new();

        let (id, mut rx) = registry.register(ns("live"), None);

        let notified = registry.notify_publish(&ns("live/stream1"), "video", 100);
        assert_eq!(notified, 1);

        let notification = rx.recv().await.unwrap();
        assert_eq!(notification.track_name, "video");
        assert_eq!(notification.track_alias, 100);

        registry.unregister(id);
    }
}
