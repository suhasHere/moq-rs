use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use moq_auth::{AuthBlob, AuthDecision, AuthHook, DenyReason, RequestContext, SessionContext};
use rsa::pss::{Signature, VerifyingKey as PssVerifyingKey};
use rsa::signature::Verifier;
use rsa::pkcs8::DecodePublicKey;
use rsa::RsaPublicKey;
use sha2::Sha384;
use tokio::sync::RwLock;

pub const PRIVACY_PASS_TOKEN_TYPE: u64 = 0x0002;

/// Token wire format (RFC 9578 §3):
///   token_type (2 bytes) = 0x0002
///   nonce (32 bytes)
///   challenge_digest (32 bytes)
///   token_key_id (32 bytes)
///   authenticator (Nk bytes, 256 for 2048-bit RSA or 512 for 4096-bit)
struct PublicToken {
    nonce: [u8; 32],
    challenge_digest: [u8; 32],
    token_key_id: [u8; 32],
    authenticator: Vec<u8>,
}

impl PublicToken {
    fn decode(data: &[u8]) -> anyhow::Result<Self> {
        // Minimum: 2 (type) + 32 (nonce) + 32 (challenge_digest) + 32 (token_key_id) + 256 (min authenticator)
        if data.len() < 98 + 256 {
            return Err(anyhow!("token too short: {} bytes", data.len()));
        }

        let token_type = u16::from_be_bytes([data[0], data[1]]);
        if token_type != 0x0002 {
            return Err(anyhow!("unexpected token type: 0x{:04x}", token_type));
        }

        let mut nonce = [0u8; 32];
        nonce.copy_from_slice(&data[2..34]);

        let mut challenge_digest = [0u8; 32];
        challenge_digest.copy_from_slice(&data[34..66]);

        let mut token_key_id = [0u8; 32];
        token_key_id.copy_from_slice(&data[66..98]);

        let authenticator = data[98..].to_vec();

        Ok(Self {
            nonce,
            challenge_digest,
            token_key_id,
            authenticator,
        })
    }

    fn authenticator_input(&self) -> Vec<u8> {
        let mut input = Vec::with_capacity(98);
        input.extend_from_slice(&0x0002u16.to_be_bytes());
        input.extend_from_slice(&self.nonce);
        input.extend_from_slice(&self.challenge_digest);
        input.extend_from_slice(&self.token_key_id);
        input
    }
}

pub struct PrivacyPassAuthHook {
    issuer_keys: Arc<RwLock<Vec<RsaPublicKey>>>,
    setup_required: bool,
}

impl PrivacyPassAuthHook {
    pub fn new(keys: Vec<RsaPublicKey>) -> Self {
        Self {
            issuer_keys: Arc::new(RwLock::new(keys)),
            setup_required: true,
        }
    }

    pub fn with_setup_required(mut self, required: bool) -> Self {
        self.setup_required = required;
        self
    }

    pub async fn update_keys(&self, keys: Vec<RsaPublicKey>) {
        let mut guard = self.issuer_keys.write().await;
        *guard = keys;
    }

    async fn verify_token(&self, token_bytes: &[u8]) -> Result<AuthDecision, AuthDecision> {
        let token =
            PublicToken::decode(token_bytes).map_err(|_| AuthDecision::deny(DenyReason::TokenMalformed))?;

        let auth_input = token.authenticator_input();
        let signature = Signature::try_from(token.authenticator.as_slice())
            .map_err(|_| AuthDecision::deny(DenyReason::TokenMalformed))?;

        let keys = self.issuer_keys.read().await;
        for key in keys.iter() {
            let verifying_key = PssVerifyingKey::<Sha384>::new(key.clone());
            if verifying_key.verify(&auth_input, &signature).is_ok() {
                return Ok(AuthDecision::allow().with_principal(Some("privacy-pass".to_string())));
            }
        }

        Err(AuthDecision::deny(DenyReason::TokenInvalid))
    }
}

#[async_trait]
impl AuthHook for PrivacyPassAuthHook {
    async fn on_setup(
        &self,
        _ctx: &SessionContext,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        let pp_blob = tokens
            .iter()
            .find(|t| t.token_type == PRIVACY_PASS_TOKEN_TYPE);

        match pp_blob {
            Some(blob) => match self.verify_token(&blob.token_value).await {
                Ok(decision) => Ok(decision),
                Err(decision) => Ok(decision),
            },
            None => {
                if self.setup_required {
                    Ok(AuthDecision::deny(DenyReason::TokenMissing))
                } else {
                    Ok(AuthDecision::allow())
                }
            }
        }
    }

    async fn on_request(
        &self,
        _ctx: &RequestContext<'_>,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        // Per-request tokens are optional; if present, verify them
        let pp_blob = tokens
            .iter()
            .find(|t| t.token_type == PRIVACY_PASS_TOKEN_TYPE);

        match pp_blob {
            Some(blob) => match self.verify_token(&blob.token_value).await {
                Ok(decision) => Ok(decision),
                Err(decision) => Ok(decision),
            },
            None => Ok(AuthDecision::allow()),
        }
    }
}

/// Fetch issuer public keys from a Privacy Pass issuer directory.
pub async fn fetch_issuer_keys(issuer_url: &str) -> anyhow::Result<Vec<RsaPublicKey>> {
    let url = format!("{}/.well-known/private-token-issuer-directory", issuer_url);
    let res = reqwest::get(&url).await?;
    let body: serde_json::Value = res.json().await?;

    let keys = body["token-keys"]
        .as_array()
        .ok_or_else(|| anyhow!("no token-keys in directory"))?;

    let mut rsa_keys = Vec::new();
    for key in keys {
        if key["token-type"].as_u64() != Some(0x0002) {
            continue;
        }
        let key_b64 = key["token-key"]
            .as_str()
            .ok_or_else(|| anyhow!("token-key not a string"))?;

        let key_bytes = URL_SAFE_NO_PAD
            .decode(key_b64)
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(key_b64))?;

        let public_key = rsa::RsaPublicKey::from_public_key_der(&key_bytes)
            .map_err(|e| anyhow!("failed to parse RSA key: {e}"))?;
        rsa_keys.push(public_key);
    }

    if rsa_keys.is_empty() {
        return Err(anyhow!("no valid RSA public keys found"));
    }

    Ok(rsa_keys)
}
