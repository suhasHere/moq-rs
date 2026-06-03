// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::HashMap;
use std::fmt;
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

    pub fn authorization_info(&self) -> MoqAuthorizationInfo {
        MoqAuthorizationInfo::from_scope(self)
    }

    pub fn encode_origin_info(&self) -> Vec<u8> {
        self.authorization_info().encode()
    }

    pub fn token_challenge(&self, issuer_name: &str) -> TokenChallenge {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&2u16.to_be_bytes());
        put_vec_u16(&mut bytes, issuer_name.as_bytes());
        put_vec_u8(&mut bytes, &[]);
        put_vec_u16(&mut bytes, &self.encode_origin_info());
        TokenChallenge::deserialize(&bytes).expect("generated token challenge is valid")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct MoqAuthorizationInfo {
    scopes: Vec<MoqAuthScope>,
}

impl MoqAuthorizationInfo {
    fn from_scope(scope: &ChallengeScope) -> Self {
        let actions = match scope.action {
            MoqAction::Setup => vec![MoqActionValue::ClientSetup],
            MoqAction::Subscribe => vec![
                MoqActionValue::Subscribe,
                MoqActionValue::Fetch,
                MoqActionValue::TrackStatus,
            ],
            MoqAction::Publish => vec![MoqActionValue::PublishNamespace, MoqActionValue::Publish],
        };
        Self {
            scopes: vec![MoqAuthScope {
                actions,
                namespace: scope.namespace.clone(),
                track: scope.track.clone(),
            }],
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut scopes = Vec::new();
        for scope in &self.scopes {
            scopes.extend(scope.encode());
        }
        let mut out = Vec::new();
        put_vec_u8(&mut out, &scopes);
        out
    }
}

impl fmt::Debug for MoqAuthorizationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoQAuthorizationInfo")
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl fmt::Display for MoqAuthorizationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Clone, Eq, PartialEq)]
struct MoqAuthScope {
    actions: Vec<MoqActionValue>,
    namespace: MatchRule<Vec<Vec<u8>>>,
    track: MatchRule<Vec<u8>>,
}

impl MoqAuthScope {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_vec_u8(
            &mut out,
            &self
                .actions
                .iter()
                .map(|action| *action as u8)
                .collect::<Vec<_>>(),
        );
        encode_namespace_match(&mut out, &self.namespace);
        encode_track_match(&mut out, &self.track);
        out
    }
}

impl fmt::Debug for MoqAuthScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MoQAuthScope")
            .field("actions", &self.actions)
            .field("namespace", &self.namespace)
            .field("track", &self.track)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum MoqActionValue {
    ClientSetup = 0,
    PublishNamespace = 2,
    Subscribe = 4,
    Publish = 6,
    Fetch = 7,
    TrackStatus = 8,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum MatchType {
    Exact = 0,
    Prefix = 1,
    Suffix = 2,
    Contains = 3,
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

fn encode_namespace_match(out: &mut Vec<u8>, rule: &MatchRule<Vec<Vec<u8>>>) {
    let (typ, tuple) = namespace_rule_parts(rule);
    out.push(typ as u8);
    let mut encoded_tuple = Vec::new();
    for field in tuple {
        put_vec_u16(&mut encoded_tuple, field);
    }
    put_vec_u16(out, &encoded_tuple);
}

fn encode_track_match(out: &mut Vec<u8>, rule: &MatchRule<Vec<u8>>) {
    let (typ, track) = track_rule_parts(rule);
    out.push(typ as u8);
    put_vec_u16(out, track);
}

fn namespace_rule_parts(rule: &MatchRule<Vec<Vec<u8>>>) -> (MatchType, &[Vec<u8>]) {
    match rule {
        MatchRule::Exact(value) => (MatchType::Exact, value),
        MatchRule::Prefix(value) => (MatchType::Prefix, value),
        MatchRule::Suffix(value) => (MatchType::Suffix, value),
        MatchRule::Contains(value) => (MatchType::Contains, value),
    }
}

fn track_rule_parts(rule: &MatchRule<Vec<u8>>) -> (MatchType, &[u8]) {
    match rule {
        MatchRule::Exact(value) => (MatchType::Exact, value),
        MatchRule::Prefix(value) => (MatchType::Prefix, value),
        MatchRule::Suffix(value) => (MatchType::Suffix, value),
        MatchRule::Contains(value) => (MatchType::Contains, value),
    }
}

fn put_vec_u8(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u8::try_from(bytes.len()).expect("vector fits in u8");
    out.push(len);
    out.extend_from_slice(bytes);
}

fn put_vec_u16(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u16::try_from(bytes.len()).expect("vector fits in u16");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
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
    fn scope_origin_info_is_draft_binary() {
        let ns = TrackNamespace::from_utf8_path("clock");
        let scope = ChallengeScope::publish_namespace_prefix(&ns);
        let encoded = scope.encode_origin_info();
        assert_eq!(usize::from(encoded[0]), encoded.len() - 1); // scopes<1..2^8-1>
        assert_eq!(encoded[1], 2); // two actions
        assert_eq!(encoded[2], MoqActionValue::PublishNamespace as u8);
        assert_eq!(encoded[3], MoqActionValue::Publish as u8);
        assert_eq!(
            format!("{}", scope.authorization_info()),
            format!("{:?}", scope.authorization_info())
        );
    }
}
