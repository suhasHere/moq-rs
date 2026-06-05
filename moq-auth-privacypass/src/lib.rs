// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Privacy Pass authentication hook for the MoQ relay.
//!
//! This crate targets the demo Privacy Pass flow first: publicly verifiable
//! Privacy Pass tokens (`0x0002`) issued by an external issuer and redeemed by
//! a single relay with in-memory replay protection.

mod challenge;
mod error;
mod stores;
mod token;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use moq_auth::{AuthBlob, AuthDecision, AuthHook, DenyReason, RequestContext, SessionContext};
use privacypass::public_tokens::server::OriginServer;

pub use challenge::{ChallengeRegistry, ChallengeScope, MatchRule, MoqAction};
pub use error::*;
pub use stores::{public_key_from_spki_der, PublicKeyStore, ReplayCache};
pub use wire::{decode_base64_reason, setup_challenge_reason, MoqAuthChallenge, MoqAuthChallengeError};

/// Outer MoQT auth token type used for Privacy Pass public tokens in the demo.
pub const MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC: u64 = 0x0002;

/// Inner Privacy Pass public token type from RFC 9578.
pub const PRIVACY_PASS_PUBLIC_TOKEN_TYPE: u16 = 0x0002;

pub struct PrivacyPassAuthHook {
    origin_server: OriginServer,
    public_keys: Arc<PublicKeyStore>,
    replay_cache: Arc<ReplayCache>,
    challenges: Arc<ChallengeRegistry>,
    setup_required: bool,
}

impl PrivacyPassAuthHook {
    pub fn new(public_keys: Arc<PublicKeyStore>, challenges: Arc<ChallengeRegistry>) -> Self {
        Self {
            origin_server: OriginServer::new(),
            public_keys,
            replay_cache: Arc::new(ReplayCache::default()),
            challenges,
            setup_required: false,
        }
    }

    pub fn with_setup_required(mut self, setup_required: bool) -> Self {
        self.setup_required = setup_required;
        self
    }

    pub fn with_replay_cache(mut self, replay_cache: Arc<ReplayCache>) -> Self {
        self.replay_cache = replay_cache;
        self
    }

    pub fn challenges(&self) -> Arc<ChallengeRegistry> {
        self.challenges.clone()
    }

    pub fn demo(public_keys: Arc<PublicKeyStore>, challenge_ttl: Duration) -> Self {
        Self::new(public_keys, Arc::new(ChallengeRegistry::new(challenge_ttl)))
    }

    fn find_token<'a>(&self, tokens: &'a [AuthBlob]) -> Option<&'a AuthBlob> {
        tokens
            .iter()
            .find(|token| token.token_type == MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC)
    }

    async fn validate_token(&self, blob: &AuthBlob) -> Result<ChallengeScope, DenyReason> {
        let token = token::decode_public_token(&blob.token_value).map_err(|err| match err {
            token::TokenDecodeError::Malformed => DenyReason::TokenMalformed,
            token::TokenDecodeError::UnsupportedType => DenyReason::TokenInvalid,
        })?;

        let challenge_digest = *token.challenge_digest();
        let digest = digest_label(&challenge_digest);
        self.origin_server
            .redeem_token(self.public_keys.as_ref(), self.replay_cache.as_ref(), token)
            .await
            .map_err(|err| {
                tracing::info!(%digest, error = %err, "Privacy Pass token rejected");
                match err {
                    privacypass::common::errors::RedeemTokenError::DoubleSpending => {
                        DenyReason::TokenReplayed
                    }
                    privacypass::common::errors::RedeemTokenError::KeyIdNotFound => {
                        DenyReason::IssuerUnknown
                    }
                    privacypass::common::errors::RedeemTokenError::TokenTypeMismatch { .. }
                    | privacypass::common::errors::RedeemTokenError::InvalidAuthenticatorLength { .. }
                    | privacypass::common::errors::RedeemTokenError::InvalidSignature { .. }
                    | privacypass::common::errors::RedeemTokenError::AuthenticatorDerivationFailed { .. }
                    | privacypass::common::errors::RedeemTokenError::AuthenticatorMismatch { .. } => {
                        DenyReason::TokenInvalid
                    }
                }
            })?;

        let scope = self
            .challenges
            .get(&challenge_digest)
            .await
            .ok_or(DenyReason::ScopeMismatch)?;
        tracing::info!(%digest, scope = %scope.authorization_info(), "Privacy Pass token redeemed");
        Ok(scope)
    }
}

fn digest_label(digest: &[u8; 32]) -> String {
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[async_trait]
impl AuthHook for PrivacyPassAuthHook {
    async fn on_setup(
        &self,
        _ctx: &SessionContext,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        let Some(blob) = self.find_token(tokens) else {
            return Ok(if self.setup_required {
                AuthDecision::deny(DenyReason::TokenMissing)
            } else {
                AuthDecision::allow()
            });
        };

        let scope = match self.validate_token(blob).await {
            Ok(scope) => scope,
            Err(reason) => return Ok(AuthDecision::deny(reason)),
        };

        if scope.allows_setup() {
            tracing::info!("Privacy Pass setup authorized");
            Ok(AuthDecision::allow())
        } else {
            tracing::info!(scope = %scope.authorization_info(), "Privacy Pass setup scope mismatch");
            Ok(AuthDecision::deny(DenyReason::ScopeMismatch))
        }
    }

    async fn on_request(
        &self,
        ctx: &RequestContext<'_>,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        let Some(blob) = self.find_token(tokens) else {
            return Ok(AuthDecision::deny(DenyReason::TokenMissing));
        };

        let scope = match self.validate_token(blob).await {
            Ok(scope) => scope,
            Err(reason) => return Ok(AuthDecision::deny(reason)),
        };

        if scope.allows(&ctx.operation) {
            tracing::info!(operation = ?ctx.operation, "Privacy Pass request authorized");
            Ok(AuthDecision::allow())
        } else {
            tracing::info!(operation = ?ctx.operation, scope = %scope.authorization_info(), "Privacy Pass request scope mismatch");
            Ok(AuthDecision::deny(DenyReason::ScopeMismatch))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use blind_rsa_signatures::reexports::rand::rng;
    use bytes::Bytes;
    use moq_auth::{AuthHook, AuthzOperation, RequestContext, SessionContext};
    use moq_transport::coding::TrackNamespace;
    use privacypass::auth::authenticate::TokenChallenge;
    use privacypass::public_tokens::server::IssuerServer;
    use privacypass::public_tokens::TokenRequest;
    use privacypass::test_utils::public_memory_store::IssuerMemoryKeyStore;
    use privacypass::TokenType;
    use tls_codec::Serialize;

    #[test]
    fn token_type_constants_match_public_privacypass() {
        assert_eq!(MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC, 0x0002);
        assert_eq!(PRIVACY_PASS_PUBLIC_TOKEN_TYPE, 0x0002);
    }

    async fn issue_token(scope: ChallengeScope) -> (PrivacyPassAuthHook, AuthBlob, ChallengeScope) {
        let issuer_keys = IssuerMemoryKeyStore::default();
        let issuer = IssuerServer::new();
        let mut rng = rng();
        let public_key = issuer.create_keypair(&mut rng, &issuer_keys).await.unwrap();

        let public_keys = Arc::new(PublicKeyStore::default());
        public_keys
            .insert_public_key(public_key.clone())
            .await
            .unwrap();

        let challenge = TokenChallenge::new(
            TokenType::Public,
            "demo-pat.issuer.cloudflare.com",
            None,
            &["moq-demo".to_string()],
        );
        let digest = challenge.digest().unwrap();

        let challenges = Arc::new(ChallengeRegistry::new(Duration::from_secs(60)));
        challenges.insert(digest, scope.clone()).await;

        let (request, state) = TokenRequest::new(&mut rng, public_key, &challenge).unwrap();
        let response = issuer
            .issue_token_response(&issuer_keys, request)
            .await
            .unwrap();
        let token = response.issue_token(&state).unwrap();
        let token_bytes = token.tls_serialize_detached().unwrap();

        let hook = PrivacyPassAuthHook::new(public_keys, challenges).with_setup_required(true);
        let blob = AuthBlob {
            token_type: MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC,
            token_value: Bytes::from(token_bytes),
        };
        (hook, blob, scope)
    }

    fn session_ctx() -> SessionContext {
        SessionContext {
            session_id: 7,
            connection_path: Some("/demo".to_string()),
            peer: "127.0.0.1:1234".parse().unwrap(),
        }
    }

    #[tokio::test]
    async fn setup_token_allows_setup() {
        let (hook, blob, _) = issue_token(ChallengeScope::setup()).await;
        let decision = hook.on_setup(&session_ctx(), &[blob]).await.unwrap();
        assert!(decision.is_allowed());
    }

    #[tokio::test]
    async fn token_replay_is_denied() {
        let (hook, blob, _) = issue_token(ChallengeScope::setup()).await;
        let ctx = session_ctx();

        assert!(hook
            .on_setup(&ctx, std::slice::from_ref(&blob))
            .await
            .unwrap()
            .is_allowed());
        let replay = hook.on_setup(&ctx, &[blob]).await.unwrap();
        assert!(!replay.is_allowed());
    }

    #[tokio::test]
    async fn subscribe_scope_allows_subscribe_only() {
        let ns = TrackNamespace::from_utf8_path("clock");
        let (hook, blob, _) = issue_token(ChallengeScope::subscribe_namespace_prefix(&ns)).await;
        let ctx = session_ctx();

        let subscribe = RequestContext {
            session: &ctx,
            operation: AuthzOperation::Subscribe {
                namespace: &ns,
                track: b"now",
            },
            request_id: Some(1),
        };
        assert!(hook
            .on_request(&subscribe, std::slice::from_ref(&blob))
            .await
            .unwrap()
            .is_allowed());

        let publish = RequestContext {
            session: &ctx,
            operation: AuthzOperation::PublishNamespace { namespace: &ns },
            request_id: Some(3),
        };
        let denied = hook.on_request(&publish, &[blob]).await.unwrap();
        assert!(!denied.is_allowed());
    }
}
