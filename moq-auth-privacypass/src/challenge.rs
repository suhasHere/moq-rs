// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::time::{Duration, Instant};

use moq_auth::AuthzOperation;
use moq_transport::coding::TrackNamespace;
use privacypass::auth::authenticate::TokenChallenge;
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

    pub fn encode_origin_info(&self) -> Vec<String> {
        let action = match self.action {
            MoqAction::Setup => "setup",
            MoqAction::Subscribe => "subscribe",
            MoqAction::Publish => "publish",
        };
        let namespace = encode_namespace_rule(&self.namespace);
        let track = encode_track_rule(&self.track);
        vec![format!(
            "moq-pp-v1;action={action};namespace={namespace};track={track}"
        )]
    }

    pub fn decode_origin_info(values: &[String]) -> Option<Self> {
        values.iter().find_map(|value| decode_scope(value))
    }

    pub fn token_challenge(&self, issuer_name: &str) -> TokenChallenge {
        TokenChallenge::new(
            privacypass::TokenType::Public,
            issuer_name,
            None,
            &self.encode_origin_info(),
        )
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

fn encode_namespace_rule(rule: &MatchRule<Vec<Vec<u8>>>) -> String {
    let (kind, value) = match rule {
        MatchRule::Exact(value) => ("exact", value),
        MatchRule::Prefix(value) => ("prefix", value),
        MatchRule::Suffix(value) => ("suffix", value),
        MatchRule::Contains(value) => ("contains", value),
    };
    let fields = value
        .iter()
        .map(|field| hex(field))
        .collect::<Vec<_>>()
        .join(".");
    format!("{kind}:{fields}")
}

fn encode_track_rule(rule: &MatchRule<Vec<u8>>) -> String {
    let (kind, value) = match rule {
        MatchRule::Exact(value) => ("exact", value),
        MatchRule::Prefix(value) => ("prefix", value),
        MatchRule::Suffix(value) => ("suffix", value),
        MatchRule::Contains(value) => ("contains", value),
    };
    format!("{kind}:{}", hex(value))
}

fn decode_scope(value: &str) -> Option<ChallengeScope> {
    let mut parts = value.split(';');
    if parts.next()? != "moq-pp-v1" {
        return None;
    }

    let mut action = None;
    let mut namespace = None;
    let mut track = None;
    for part in parts {
        let (key, value) = part.split_once('=')?;
        match key {
            "action" => {
                action = Some(match value {
                    "setup" => MoqAction::Setup,
                    "subscribe" => MoqAction::Subscribe,
                    "publish" => MoqAction::Publish,
                    _ => return None,
                });
            }
            "namespace" => namespace = Some(decode_namespace_rule(value)?),
            "track" => track = Some(decode_track_rule(value)?),
            _ => {}
        }
    }

    Some(ChallengeScope {
        action: action?,
        namespace: namespace?,
        track: track?,
    })
}

fn decode_namespace_rule(value: &str) -> Option<MatchRule<Vec<Vec<u8>>>> {
    let (kind, raw) = value.split_once(':')?;
    let tuple = if raw.is_empty() {
        vec![]
    } else {
        raw.split('.').map(unhex).collect::<Option<Vec<_>>>()?
    };
    let rule = match kind {
        "exact" => MatchRule::Exact(tuple),
        "prefix" => MatchRule::Prefix(tuple),
        "suffix" => MatchRule::Suffix(tuple),
        "contains" => MatchRule::Contains(tuple),
        _ => return None,
    };
    Some(rule)
}

fn decode_track_rule(value: &str) -> Option<MatchRule<Vec<u8>>> {
    let (kind, raw) = value.split_once(':')?;
    let bytes = unhex(raw)?;
    let rule = match kind {
        "exact" => MatchRule::Exact(bytes),
        "prefix" => MatchRule::Prefix(bytes),
        "suffix" => MatchRule::Suffix(bytes),
        "contains" => MatchRule::Contains(bytes),
        _ => return None,
    };
    Some(rule)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks(2)
        .map(|chunk| {
            let text = std::str::from_utf8(chunk).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
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

    #[test]
    fn scope_origin_info_round_trips() {
        let ns = TrackNamespace::from_utf8_path("clock");
        let scope = ChallengeScope::publish_namespace_prefix(&ns);
        let decoded = ChallengeScope::decode_origin_info(&scope.encode_origin_info()).unwrap();
        assert!(decoded.allows(&AuthzOperation::PublishNamespace { namespace: &ns }));
    }
}
