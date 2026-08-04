use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use blind_rsa_signatures::pbrsa::PartiallyBlindPublicKey;
use blind_rsa_signatures::{Deterministic, PSSZero, Sha384};
use moq_auth::{AuthBlob, AuthDecision, AuthHook, DenyReason, RequestContext, SessionContext};
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::pss::{Signature, VerifyingKey as PssVerifyingKey};
use rsa::signature::Verifier;
use rsa::RsaPublicKey;
use sha2::Sha384 as RsaSha384;
use spki::SubjectPublicKeyInfoRef;
use tokio::sync::RwLock;

pub const TOKEN_TYPE_BLIND_RSA: u64 = 0x0002;
pub const TOKEN_TYPE_PBRS: u64 = 0xda7a;

type PbPk = PartiallyBlindPublicKey<Sha384, PSSZero, Deterministic>;

/// Token wire format (RFC 9578 §3):
///   token_type (2 bytes)
///   nonce (32 bytes)
///   challenge_digest (32 bytes)
///   token_key_id (32 bytes)
///   authenticator (Nk bytes, 256 for 2048-bit RSA)
struct PublicToken {
    token_type: u16,
    nonce: [u8; 32],
    challenge_digest: [u8; 32],
    token_key_id: [u8; 32],
    authenticator: Vec<u8>,
}

impl PublicToken {
    fn decode(data: &[u8]) -> anyhow::Result<(Self, usize)> {
        if data.len() < 98 + 256 {
            return Err(anyhow!("token too short: {} bytes", data.len()));
        }

        let token_type = u16::from_be_bytes([data[0], data[1]]);

        let mut nonce = [0u8; 32];
        nonce.copy_from_slice(&data[2..34]);

        let mut challenge_digest = [0u8; 32];
        challenge_digest.copy_from_slice(&data[34..66]);

        let mut token_key_id = [0u8; 32];
        token_key_id.copy_from_slice(&data[66..98]);

        let authenticator = data[98..98 + 256].to_vec();

        Ok((
            Self {
                token_type,
                nonce,
                challenge_digest,
                token_key_id,
                authenticator,
            },
            98 + 256,
        ))
    }

    fn authenticator_input(&self) -> Vec<u8> {
        let mut input = Vec::with_capacity(98);
        input.extend_from_slice(&self.token_type.to_be_bytes());
        input.extend_from_slice(&self.nonce);
        input.extend_from_slice(&self.challenge_digest);
        input.extend_from_slice(&self.token_key_id);
        input
    }
}

pub struct PrivacyPassAuthHook {
    blind_rsa_keys: Arc<RwLock<Vec<RsaPublicKey>>>,
    pbrs_keys: Arc<RwLock<Vec<PbPk>>>,
    setup_required: bool,
}

impl PrivacyPassAuthHook {
    pub fn new(blind_rsa_keys: Vec<RsaPublicKey>, pbrs_keys: Vec<PbPk>) -> Self {
        Self {
            blind_rsa_keys: Arc::new(RwLock::new(blind_rsa_keys)),
            pbrs_keys: Arc::new(RwLock::new(pbrs_keys)),
            setup_required: true,
        }
    }

    pub fn with_setup_required(mut self, required: bool) -> Self {
        self.setup_required = required;
        self
    }

    pub async fn update_keys(&self, blind_rsa_keys: Vec<RsaPublicKey>, pbrs_keys: Vec<PbPk>) {
        let mut guard = self.blind_rsa_keys.write().await;
        *guard = blind_rsa_keys;
        drop(guard);
        let mut guard = self.pbrs_keys.write().await;
        *guard = pbrs_keys;
    }

    async fn verify_token(&self, token_bytes: &[u8]) -> Result<AuthDecision, AuthDecision> {
        if token_bytes.len() < 2 {
            return Err(AuthDecision::deny(DenyReason::TokenMalformed));
        }

        let token_type = u16::from_be_bytes([token_bytes[0], token_bytes[1]]);

        match token_type {
            0x0002 => self.verify_blind_rsa(token_bytes).await,
            0xda7a => self.verify_pbrs(token_bytes).await,
            _ => Err(AuthDecision::deny(DenyReason::TokenMalformed)),
        }
    }

    async fn verify_blind_rsa(&self, token_bytes: &[u8]) -> Result<AuthDecision, AuthDecision> {
        let (token, _) = PublicToken::decode(token_bytes)
            .map_err(|_| AuthDecision::deny(DenyReason::TokenMalformed))?;

        let auth_input = token.authenticator_input();
        let signature = Signature::try_from(token.authenticator.as_slice())
            .map_err(|_| AuthDecision::deny(DenyReason::TokenMalformed))?;

        let keys = self.blind_rsa_keys.read().await;
        for key in keys.iter() {
            let verifying_key = PssVerifyingKey::<RsaSha384>::new_with_salt_len(key.clone(), 0);
            if verifying_key.verify(&auth_input, &signature).is_ok() {
                return Ok(AuthDecision::allow().with_principal(Some("privacy-pass".to_string())));
            }
        }

        Err(AuthDecision::deny(DenyReason::TokenInvalid))
    }

    async fn verify_pbrs(&self, token_bytes: &[u8]) -> Result<AuthDecision, AuthDecision> {
        let (token, token_len) = PublicToken::decode(token_bytes)
            .map_err(|_| AuthDecision::deny(DenyReason::TokenMalformed))?;

        // Extensions are appended after the token
        if token_bytes.len() <= token_len {
            tracing::warn!("PBRS token missing extensions");
            return Err(AuthDecision::deny(DenyReason::TokenMalformed));
        }
        let extensions_bytes = &token_bytes[token_len..];

        let auth_input = token.authenticator_input();
        let signature = Signature::try_from(token.authenticator.as_slice())
            .map_err(|_| AuthDecision::deny(DenyReason::TokenMalformed))?;

        let keys = self.pbrs_keys.read().await;
        for pk in keys.iter() {
            let derived = match pk.derive_public_key_for_metadata(extensions_bytes) {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Round-trip through DER to bridge rsa crate versions
            let derived_der = match derived.to_der() {
                Ok(d) => d,
                Err(_) => continue,
            };
            let rsa_pk = match RsaPublicKey::from_public_key_der(&derived_der) {
                Ok(k) => k,
                Err(_) => match SubjectPublicKeyInfoRef::try_from(derived_der.as_slice()) {
                    Ok(spki) => {
                        match RsaPublicKey::from_pkcs1_der(spki.subject_public_key.raw_bytes()) {
                            Ok(k) => k,
                            Err(_) => continue,
                        }
                    }
                    Err(_) => continue,
                },
            };

            let verifying_key = PssVerifyingKey::<RsaSha384>::new_with_salt_len(rsa_pk, 0);

            if verifying_key.verify(&auth_input, &signature).is_ok() {
                let scope = parse_moq_scope(extensions_bytes);
                let mut decision =
                    AuthDecision::allow().with_principal(Some("privacy-pass-pbrs".to_string()));
                if let Some(s) = scope {
                    decision = decision.with_scope(Some(s));
                }
                return Ok(decision);
            }
        }

        Err(AuthDecision::deny(DenyReason::TokenInvalid))
    }
}

/// Parse MoQ action scope from extensions bytes.
/// Format: extensions_length(2) + [extension_type(2) + data_length(2) + data]*
fn parse_moq_scope(extensions_bytes: &[u8]) -> Option<String> {
    if extensions_bytes.len() < 2 {
        return None;
    }
    let total_len = u16::from_be_bytes([extensions_bytes[0], extensions_bytes[1]]) as usize;
    if extensions_bytes.len() < 2 + total_len {
        return None;
    }

    let mut offset = 2;
    while offset + 4 <= 2 + total_len {
        let ext_type = u16::from_be_bytes([extensions_bytes[offset], extensions_bytes[offset + 1]]);
        let data_len =
            u16::from_be_bytes([extensions_bytes[offset + 2], extensions_bytes[offset + 3]])
                as usize;
        offset += 4;

        if offset + data_len > extensions_bytes.len() {
            break;
        }

        if ext_type == 0x0001 {
            // MoQ actions extension — JSON-encoded scope
            if let Ok(scope_str) = std::str::from_utf8(&extensions_bytes[offset..offset + data_len])
            {
                return Some(scope_str.to_string());
            }
        }
        offset += data_len;
    }
    None
}

#[async_trait]
impl AuthHook for PrivacyPassAuthHook {
    async fn on_setup(
        &self,
        _ctx: &SessionContext,
        tokens: &[AuthBlob],
    ) -> anyhow::Result<AuthDecision> {
        // Look for either token type
        let pp_blob = tokens
            .iter()
            .find(|t| t.token_type == TOKEN_TYPE_BLIND_RSA || t.token_type == TOKEN_TYPE_PBRS);

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
        let pp_blob = tokens
            .iter()
            .find(|t| t.token_type == TOKEN_TYPE_BLIND_RSA || t.token_type == TOKEN_TYPE_PBRS);

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
/// Returns (blind_rsa_keys, pbrs_keys).
pub async fn fetch_issuer_keys(issuer_url: &str) -> anyhow::Result<(Vec<RsaPublicKey>, Vec<PbPk>)> {
    let url = format!("{}/.well-known/private-token-issuer-directory", issuer_url);
    let res = reqwest::get(&url).await?;
    let body: serde_json::Value = res.json().await?;

    let keys = body["token-keys"]
        .as_array()
        .ok_or_else(|| anyhow!("no token-keys in directory"))?;

    let mut blind_rsa_keys = Vec::new();
    let mut pbrs_keys = Vec::new();

    for key in keys {
        let token_type = key["token-type"].as_u64().unwrap_or(0);
        let key_b64 = match key["token-key"].as_str() {
            Some(k) => k,
            None => continue,
        };

        let key_bytes = URL_SAFE_NO_PAD
            .decode(key_b64)
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(key_b64))?;

        match token_type {
            0x0002 => {
                let public_key = match RsaPublicKey::from_public_key_der(&key_bytes) {
                    Ok(k) => k,
                    Err(_) => {
                        let spki = SubjectPublicKeyInfoRef::try_from(key_bytes.as_slice())
                            .map_err(|e| anyhow!("failed to parse SPKI: {e}"))?;
                        RsaPublicKey::from_pkcs1_der(spki.subject_public_key.raw_bytes())
                            .map_err(|e| anyhow!("failed to parse RSA key from SPKI: {e}"))?
                    }
                };
                blind_rsa_keys.push(public_key);
            }
            0xda7a => {
                let pk = PbPk::from_der(&key_bytes)
                    .map_err(|e| anyhow!("failed to parse PBRS key: {e}"))?;
                pbrs_keys.push(pk);
            }
            _ => {}
        }
    }

    if blind_rsa_keys.is_empty() && pbrs_keys.is_empty() {
        return Err(anyhow!("no valid issuer keys found"));
    }

    tracing::info!(
        "Loaded {} Blind RSA key(s) and {} PBRS key(s)",
        blind_rsa_keys.len(),
        pbrs_keys.len()
    );

    Ok((blind_rsa_keys, pbrs_keys))
}
