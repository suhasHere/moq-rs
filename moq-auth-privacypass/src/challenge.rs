// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::time::{Duration, Instant};

use moq_auth::AuthzOperation;
use moq_transport::coding::TrackNamespace;
use privacypass::ChallengeDigest;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoqAction {
    Setup,
    Subscribe,
    Publish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchRule<T> {
    Exact(T),
    Prefix(T),
    Suffix(T),
    Contains(T),
}

impl MatchRule<Vec<Vec<u8>>> {
    fn matches_namespace(&self, target: &[Vec<u8>]) -> bool {
        match self {
            Self::Exact(pattern) => target == pattern.as_slice(),
            Self::Prefix(pattern) => target.starts_with(pattern),
            Self::Suffix(pattern) => target.ends_with(pattern),
            Self::Contains(pattern) => {
                pattern.is_empty() || target.windows(pattern.len()).any(|w| w == pattern)
            }
        }
    }
}

impl MatchRule<Vec<u8>> {
    fn matches_track(&self, target: &[u8]) -> bool {
        match self {
            Self::Exact(pattern) => target == pattern.as_slice(),
            Self::Prefix(pattern) => target.starts_with(pattern),
            Self::Suffix(pattern) => target.ends_with(pattern),
            Self::Contains(pattern) => {
                pattern.is_empty() || target.windows(pattern.len()).any(|w| w == pattern)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChallengeScope {
    action: MoqAction,
    namespace: MatchRule<Vec<Vec<u8>>>,
    track: MatchRule<Vec<u8>>,
}

impl ChallengeScope {
    pub fn setup() -> Self {
        Self {
            action: MoqAction::Setup,
            namespace: MatchRule::Prefix(vec![]),
            track: MatchRule::Prefix(vec![]),
        }
    }

    pub fn subscribe_namespace_prefix(namespace: &TrackNamespace) -> Self {
        Self {
            action: MoqAction::Subscribe,
            namespace: MatchRule::Prefix(namespace_to_tuple(namespace)),
            track: MatchRule::Prefix(vec![]),
        }
    }

    pub fn publish_namespace_prefix(namespace: &TrackNamespace) -> Self {
        Self {
            action: MoqAction::Publish,
            namespace: MatchRule::Prefix(namespace_to_tuple(namespace)),
            track: MatchRule::Prefix(vec![]),
        }
    }

    pub fn allows(&self, operation: &AuthzOperation<'_>) -> bool {
        match operation {
            AuthzOperation::Subscribe { namespace, track }
            | AuthzOperation::Fetch { namespace, track }
            | AuthzOperation::TrackStatus { namespace, track } => {
                self.action == MoqAction::Subscribe
                    && self
                        .namespace
                        .matches_namespace(&namespace_to_tuple(namespace))
                    && self.track.matches_track(track)
            }
            AuthzOperation::Publish { namespace, track } => {
                self.action == MoqAction::Publish
                    && self
                        .namespace
                        .matches_namespace(&namespace_to_tuple(namespace))
                    && self.track.matches_track(track)
            }
            AuthzOperation::PublishNamespace { namespace }
            | AuthzOperation::PublishNamespaceDone { namespace } => {
                self.action == MoqAction::Publish
                    && self
                        .namespace
                        .matches_namespace(&namespace_to_tuple(namespace))
            }
            _ => false,
        }
    }

    pub fn allows_setup(&self) -> bool {
        self.action == MoqAction::Setup
    }
}

#[derive(Debug, Clone)]
struct ChallengeEntry {
    scope: ChallengeScope,
    expires_at: Instant,
}

#[derive(Debug)]
pub struct ChallengeRegistry {
    ttl: Duration,
    entries: RwLock<HashMap<ChallengeDigest, ChallengeEntry>>,
}

impl ChallengeRegistry {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: RwLock::new(HashMap::new()),
        }
    }

    pub async fn insert(&self, digest: ChallengeDigest, scope: ChallengeScope) {
        let mut entries = self.entries.write().await;
        entries.insert(
            digest,
            ChallengeEntry {
                scope,
                expires_at: Instant::now() + self.ttl,
            },
        );
    }

    pub async fn get(&self, digest: &ChallengeDigest) -> Option<ChallengeScope> {
        let now = Instant::now();
        let mut entries = self.entries.write().await;
        entries.retain(|_, entry| entry.expires_at > now);
        entries.get(digest).map(|entry| entry.scope.clone())
    }
}

fn namespace_to_tuple(namespace: &TrackNamespace) -> Vec<Vec<u8>> {
    namespace
        .fields
        .iter()
        .map(|field| field.value.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_prefix_matches_on_tuple_boundaries() {
        let scope = ChallengeScope::subscribe_namespace_prefix(&TrackNamespace::from_utf8_path(
            "sports/live",
        ));
        let ok = TrackNamespace::from_utf8_path("sports/live/soccer");
        let wrong = TrackNamespace::from_utf8_path("sports/livestream");

        assert!(scope.allows(&AuthzOperation::Subscribe {
            namespace: &ok,
            track: b"video"
        }));
        assert!(!scope.allows(&AuthzOperation::Subscribe {
            namespace: &wrong,
            track: b"video"
        }));
    }
}
