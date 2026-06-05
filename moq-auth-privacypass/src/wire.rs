// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use privacypass::auth::authenticate::TokenChallenge;
use tls_codec::{Deserialize, Serialize, TlsByteVecU16};

use crate::ChallengeScope;

pub fn setup_challenge_reason(issuer_name: &str) -> Result<String, MoqAuthChallengeError> {
    let bytes = MoqAuthChallenge::new(vec![ChallengeScope::setup().token_challenge(issuer_name)])?
        .encode_for_reason_phrase()?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoqAuthChallenge {
    challenges: Vec<TokenChallenge>,
}

impl MoqAuthChallenge {
    pub fn new(challenges: Vec<TokenChallenge>) -> Result<Self, MoqAuthChallengeError> {
        if challenges.is_empty() {
            return Err(MoqAuthChallengeError::Empty);
        }
        Ok(Self { challenges })
    }

    pub fn challenges(&self) -> &[TokenChallenge] {
        &self.challenges
    }

    pub fn encode_for_reason_phrase(&self) -> Result<Vec<u8>, MoqAuthChallengeError> {
        // draft-ietf-moq-privacy-pass-auth-02 requires MoQAuthChallenge bytes
        // in the MoQT reason phrase. draft-ietf-moq-transport-18 defines the
        // reason phrase as UTF-8 diagnostic text. For this interop demo we
        // intentionally follow the Privacy Pass draft and carry raw bytes.
        // Callers must not treat this field as human-readable text.
        let mut encoded_challenges = Vec::new();
        for challenge in &self.challenges {
            encoded_challenges.extend(
                challenge
                    .serialize()
                    .map_err(|_| MoqAuthChallengeError::Encode)?,
            );
        }
        let vec = TlsByteVecU16::from(encoded_challenges);
        vec.tls_serialize_detached()
            .map_err(|_| MoqAuthChallengeError::Encode)
    }

    pub fn decode_from_reason_phrase(mut bytes: &[u8]) -> Result<Self, MoqAuthChallengeError> {
        let raw = TlsByteVecU16::tls_deserialize(&mut bytes)
            .map_err(|_| MoqAuthChallengeError::Decode)?;
        if !bytes.is_empty() {
            return Err(MoqAuthChallengeError::Decode);
        }

        let mut remaining = raw.as_slice();
        let mut challenges = Vec::new();
        while !remaining.is_empty() {
            let before = remaining.len();
            let challenge = TokenChallenge::tls_deserialize(&mut remaining)
                .map_err(|_| MoqAuthChallengeError::Decode)?;
            if remaining.len() == before {
                return Err(MoqAuthChallengeError::Decode);
            }
            challenges.push(challenge);
        }

        Self::new(challenges)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MoqAuthChallengeError {
    #[error("moq auth challenge list is empty")]
    Empty,
    #[error("could not encode moq auth challenge")]
    Encode,
    #[error("could not decode moq auth challenge")]
    Decode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use privacypass::TokenType;

    #[test]
    fn auth_challenge_round_trips_raw_reason_bytes() {
        let challenge = TokenChallenge::new(
            TokenType::Public,
            "demo-pat.issuer.cloudflare.com",
            None,
            &["moq-demo".to_string()],
        );
        let auth = MoqAuthChallenge::new(vec![challenge.clone()]).unwrap();
        let encoded = auth.encode_for_reason_phrase().unwrap();
        let decoded = MoqAuthChallenge::decode_from_reason_phrase(&encoded).unwrap();
        assert_eq!(decoded.challenges(), &[challenge]);
    }
}
