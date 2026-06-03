// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Privacy Pass authentication hook for the MoQ relay.
//!
//! This crate targets the demo Privacy Pass flow first: publicly verifiable
//! Privacy Pass tokens (`0x0002`) issued by an external issuer and redeemed by
//! a single relay with in-memory replay protection.

/// Outer MoQT auth token type used for Privacy Pass public tokens in the demo.
pub const MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC: u64 = 0x0002;

/// Inner Privacy Pass public token type from RFC 9578.
pub const PRIVACY_PASS_PUBLIC_TOKEN_TYPE: u16 = 0x0002;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_type_constants_match_public_privacypass() {
        assert_eq!(MOQ_AUTH_TOKEN_TYPE_PRIVACY_PASS_PUBLIC, 0x0002);
        assert_eq!(PRIVACY_PASS_PUBLIC_TOKEN_TYPE, 0x0002);
    }
}
