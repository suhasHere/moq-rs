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
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Deserialize;
use tls_codec::{Deserialize as _, Serialize as _};
use url::Url;
use web_transport::quinn::{SessionError as WtSessionError, WebTransportError};

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

const TOKEN_REQUEST: &str = "application/private-token-request";
const TOKEN_RESPONSE: &str = "application/private-token-response";

pub async fn setup_auth_from_error(
    issuer: &Url,
    err: &moq_transport::session::SessionError,
    disable_verify: bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(reason) = setup_close_reason(err) else {
        return Ok(None);
    };
    let challenge = moq_auth_privacypass::decode_base64_reason(reason)?;
    let Some(token_challenge) = challenge.challenges().first() else {
        return Ok(None);
    };
    tracing::info!("minting Privacy Pass setup token from MoQ close challenge");
    let token = mint_token_for_challenge(issuer, token_challenge, disable_verify).await?;
    Ok(Some(encode_auth_token(
        MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC,
        &token,
    )))
}

pub async fn token_params_from_error(
    issuer: &Url,
    err: &moq_transport::serve::ServeError,
    disable_verify: bool,
) -> anyhow::Result<Option<KeyValuePairs>> {
    let moq_transport::serve::ServeError::ClosedWithReason { reason, .. } = err else {
        return Ok(None);
    };
    let challenge = moq_auth_privacypass::decode_base64_reason(reason)?;
    let Some(token_challenge) = challenge.challenges().first() else {
        return Ok(None);
    };
    tracing::info!("minting Privacy Pass request token from MoQ error challenge");
    let token = mint_token_for_challenge(issuer, token_challenge, disable_verify).await?;
    let mut params = KeyValuePairs::new();
    params.set_bytesvalue(
        moq_transport::setup::ParameterType::AuthorizationToken.into(),
        encode_auth_token(MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC, &token),
    );
    Ok(Some(params))
}

async fn mint_token_for_challenge(
    issuer: &Url,
    challenge: &TokenChallenge,
    disable_verify: bool,
) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(disable_verify)
        .build()?;

    let directory_url = issuer.join("/.well-known/private-token-issuer-directory")?;
    tracing::info!(url = %directory_url, "fetching Privacy Pass issuer directory");
    let directory: IssuerDirectory = client
        .get(directory_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    mint_token_for_challenge_with_directory(issuer, &directory, challenge, &client).await
}

async fn mint_token_for_challenge_with_directory(
    issuer: &Url,
    directory: &IssuerDirectory,
    challenge: &TokenChallenge,
    client: &reqwest::Client,
) -> anyhow::Result<Vec<u8>> {
    let key = directory
        .token_keys
        .iter()
        .find(|key| key.token_type == moq_auth_privacypass::PRIVACY_PASS_PUBLIC_TOKEN_TYPE)
        .ok_or_else(|| anyhow::anyhow!("issuer has no public Privacy Pass token key"))?;
    tracing::info!("selected Privacy Pass public token key");
    let key_der = URL_SAFE_NO_PAD.decode(key.token_key.as_bytes())?;
    let public_key = PublicKey::<Sha384, PSS, Deterministic>::from_spki(&key_der)
        .map_err(|e| anyhow::anyhow!("invalid issuer key: {e}"))?;

    let mut rng = rng();
    let (request, state) = TokenRequest::new(&mut rng, public_key, challenge)?;
    let request_body = request.tls_serialize_detached()?;
    let response_body = client
        .post(issuer.join(&directory.request_uri)?)
        .header(CONTENT_TYPE, TOKEN_REQUEST)
        .header(ACCEPT, TOKEN_RESPONSE)
        .body(request_body)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    tracing::info!("received Privacy Pass token response");
    let response =
        privacypass::public_tokens::TokenResponse::tls_deserialize(&mut response_body.as_ref())?;
    let token = response.issue_token(&state)?.tls_serialize_detached()?;
    tracing::info!(bytes = token.len(), "finalized Privacy Pass token");
    Ok(token)
}

fn setup_close_reason(err: &moq_transport::session::SessionError) -> Option<&str> {
    match err {
        moq_transport::session::SessionError::WebTransport(web_transport::Error::Session(
            WtSessionError::WebTransportError(WebTransportError::Closed(2, reason)),
        )) => Some(reason.as_str()),
        _ => None,
    }
}
