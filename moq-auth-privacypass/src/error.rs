// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc. and contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use moq_auth::DenyReason;

pub const TOKEN_MISSING: u64 = 0x0100;
pub const TOKEN_INVALID: u64 = 0x0101;
pub const TOKEN_EXPIRED: u64 = 0x0102;
pub const TOKEN_REPLAYED: u64 = 0x0103;
pub const SCOPE_MISMATCH: u64 = 0x0104;
pub const ISSUER_UNKNOWN: u64 = 0x0105;
pub const TOKEN_MALFORMED: u64 = 0x0106;

pub fn error_code(reason: &DenyReason) -> u64 {
    match reason {
        DenyReason::TokenMissing => TOKEN_MISSING,
        DenyReason::TokenInvalid => TOKEN_INVALID,
        DenyReason::TokenExpired => TOKEN_EXPIRED,
        DenyReason::TokenReplayed => TOKEN_REPLAYED,
        DenyReason::ScopeMismatch => SCOPE_MISMATCH,
        DenyReason::IssuerUnknown => ISSUER_UNKNOWN,
        DenyReason::TokenMalformed => TOKEN_MALFORMED,
        DenyReason::Other { .. } => TOKEN_INVALID,
    }
}
