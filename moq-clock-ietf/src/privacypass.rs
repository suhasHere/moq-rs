// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use blind_rsa_signatures::reexports::rand::rng;
use blind_rsa_signatures::{Deterministic, PublicKey, Sha384, PSS};
use moq_auth_privacypass::MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC;
use moq_transport::coding::KeyValuePairs;
use moq_transport::session::encode_auth_token;
use privacypass::auth::authenticate::TokenChallenge;
use privacypass::public_tokens::TokenRequest;
use serde::Deserialize;
use tls_codec::{Deserialize as _, Serialize as _};
use url::Url;

#[derive(Debug, Deserialize)]
struct IssuerDirectory {
    #[serde(rename = "issuer-request-uri")]
    request_uri: String,
    #[serde(rename = "token-keys")]
    token_keys: Vec<IssuerTokenKey>,
}

#[derive(Debug, Deserialize)]
struct IssuerTokenKey {
    #[serde(rename = "token-type")]
    token_type: u16,
    #[serde(rename = "token-key")]
    token_key: String,
}

#[derive(Debug, Deserialize)]
struct ChallengeResponse {
    challenge: Vec<u8>,
}

pub async fn token_params(
    issuer: &Url,
    relay: &Url,
    action: &str,
    namespace: &str,
) -> anyhow::Result<KeyValuePairs> {
    let token = mint_token(issuer, relay, action, namespace).await?;
    let mut params = KeyValuePairs::new();
    params.set_bytesvalue(
        moq_transport::setup::ParameterType::AuthorizationToken.into(),
        encode_auth_token(MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC, &token),
    );
    Ok(params)
}

pub async fn setup_auth(issuer: &Url, relay: &Url) -> anyhow::Result<Vec<u8>> {
    let token = mint_token(issuer, relay, "setup", "").await?;
    Ok(encode_auth_token(
        MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC,
        &token,
    ))
}

async fn mint_token(
    issuer: &Url,
    relay: &Url,
    action: &str,
    namespace: &str,
) -> anyhow::Result<Vec<u8>> {
    let directory_url = issuer.join("/.well-known/private-token-issuer-directory")?;
    let directory: IssuerDirectory = reqwest::get(directory_url)
        .await?
        .error_for_status()?
        .json()
        .await?;
    let key = directory
        .token_keys
        .into_iter()
        .find(|key| key.token_type == moq_auth_privacypass::PRIVACY_PASS_PUBLIC_TOKEN_TYPE)
        .ok_or_else(|| anyhow::anyhow!("issuer has no public Privacy Pass token key"))?;
    let key_der = URL_SAFE_NO_PAD.decode(key.token_key.as_bytes())?;
    let public_key = PublicKey::<Sha384, PSS, Deterministic>::from_spki(&key_der)
        .map_err(|e| anyhow::anyhow!("invalid issuer key: {e}"))?;

    let mut challenge_url = relay.join("/pp/challenge")?;
    challenge_url
        .query_pairs_mut()
        .append_pair("action", action)
        .append_pair("namespace", namespace);
    let challenge_response: ChallengeResponse = reqwest::get(challenge_url)
        .await?
        .error_for_status()?
        .json()
        .await?;
    let challenge = TokenChallenge::deserialize(&challenge_response.challenge)?;

    let mut rng = rng();
    let (request, state) = TokenRequest::new(&mut rng, public_key, &challenge)?;
    let request_body = request.tls_serialize_detached()?;
    let response_body = reqwest::Client::new()
        .post(issuer.join(&directory.request_uri)?)
        .body(request_body)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let response =
        privacypass::public_tokens::TokenResponse::tls_deserialize(&mut response_body.as_ref())?;
    Ok(response.issue_token(&state)?.tls_serialize_detached()?)
}
