// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use privacypass::auth::authorize::Token;
use privacypass::public_tokens::PublicToken;
use privacypass::TokenType;
use tls_codec::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum TokenDecodeError {
    #[error("token is malformed")]
    Malformed,
    #[error("unsupported token type")]
    UnsupportedType,
}

pub fn decode_public_token(mut bytes: &[u8]) -> Result<PublicToken, TokenDecodeError> {
    let token = Token::tls_deserialize(&mut bytes).map_err(|_| TokenDecodeError::Malformed)?;
    if !bytes.is_empty() {
        return Err(TokenDecodeError::Malformed);
    }
    if token.token_type() != TokenType::Public {
        return Err(TokenDecodeError::UnsupportedType);
    }
    Ok(token)
}
