//! DTS (Dynamic Track Switching) support per draft-wilaw-moq-dts4moq
//!
//! Enables relays to dynamically select which track to forward from a switching set
//! based on available downstream bandwidth. A switching set is a collection of
//! time-aligned MoQ tracks representing the same content at different throughput levels.

use crate::coding::{Decode, DecodeError, Encode, EncodeError, KeyValuePairs, Value};
use crate::message::ParameterType;
use bytes::{Buf, BufMut};

/// Assignment of a track to a switching set for ABR selection.
///
/// Wire format (inside BytesValue for parameter 0x41):
/// ```text
/// SWITCHING-SET-ASSIGNMENT {
///   Switching set ID (v64),
///   Throughput threshold (v64),    // kbps
///   Set throughput fraction (v64), // relative weight 1-10
///   Flags (1 byte),                // bit 0 = activate
///   [Set rank (1 byte)]            // optional, 1-255, lower = higher priority
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchingSetAssignment {
    /// Unique identifier for the switching set
    pub set_id: u64,
    /// Minimum throughput in kbps required to select this track
    pub throughput_kbps: u64,
    /// Relative weight for bandwidth allocation (1-10)
    pub fraction: u64,
    /// When true, activate switching for this set (all tracks registered)
    pub activate: bool,
    /// Degradation priority (1-255, lower = higher priority). Default is 1.
    pub rank: Option<u8>,
}

impl Default for SwitchingSetAssignment {
    fn default() -> Self {
        Self {
            set_id: 0,
            throughput_kbps: 0,
            fraction: 1,
            activate: false,
            rank: None,
        }
    }
}

impl SwitchingSetAssignment {
    pub fn new(set_id: u64, throughput_kbps: u64) -> Self {
        Self {
            set_id,
            throughput_kbps,
            ..Default::default()
        }
    }

    pub fn with_fraction(mut self, fraction: u64) -> Self {
        self.fraction = fraction;
        self
    }

    pub fn with_rank(mut self, rank: u8) -> Self {
        self.rank = Some(rank);
        self
    }

    pub fn with_activate(mut self, activate: bool) -> Self {
        self.activate = activate;
        self
    }

    /// Encode to bytes for use in KeyValuePairs (parameter 0x41 is odd, so BytesValue)
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.encode(&mut buf).expect("encoding to vec cannot fail");
        buf
    }

    /// Decode from bytes extracted from KeyValuePairs
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut buf = bytes;
        Self::decode(&mut buf)
    }

    /// Get the effective rank (defaults to 1 if not specified)
    pub fn effective_rank(&self) -> u8 {
        self.rank.unwrap_or(1)
    }
}

impl Encode for SwitchingSetAssignment {
    fn encode<W: BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.set_id.encode(w)?;
        self.throughput_kbps.encode(w)?;
        self.fraction.encode(w)?;

        // Flags byte: bit 0 = activate
        let mut flags: u8 = 0;
        if self.activate {
            flags |= 0x01;
        }
        // bit 1 indicates rank is present
        if self.rank.is_some() {
            flags |= 0x02;
        }
        w.put_u8(flags);

        // Optional rank
        if let Some(rank) = self.rank {
            if rank == 0 {
                return Err(EncodeError::InvalidValue);
            }
            w.put_u8(rank);
        }

        Ok(())
    }
}

impl Decode for SwitchingSetAssignment {
    fn decode<R: Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let set_id = u64::decode(r)?;
        let throughput_kbps = u64::decode(r)?;
        let fraction = u64::decode(r)?;

        if r.remaining() < 1 {
            return Err(DecodeError::More(1));
        }
        let flags = r.get_u8();
        let activate = (flags & 0x01) != 0;
        let has_rank = (flags & 0x02) != 0;

        let rank = if has_rank {
            if r.remaining() < 1 {
                return Err(DecodeError::More(1));
            }
            let rank = r.get_u8();
            if rank == 0 {
                return Err(DecodeError::InvalidValue);
            }
            Some(rank)
        } else {
            None
        };

        Ok(Self {
            set_id,
            throughput_kbps,
            fraction,
            activate,
            rank,
        })
    }
}

/// Extension trait for KeyValuePairs to work with DTS parameters
pub trait DtsParams {
    /// Get the switching set assignment if present
    fn get_switching_set_assignment(&self) -> Option<SwitchingSetAssignment>;

    /// Set the switching set assignment
    fn set_switching_set_assignment(&mut self, assignment: &SwitchingSetAssignment);

    /// Check if this has a switching set assignment
    fn has_switching_set_assignment(&self) -> bool;
}

impl DtsParams for KeyValuePairs {
    fn get_switching_set_assignment(&self) -> Option<SwitchingSetAssignment> {
        let key = ParameterType::SwitchingSetAssignment as u64;
        self.get(key).and_then(|kvp| match &kvp.value {
            Value::BytesValue(bytes) => SwitchingSetAssignment::from_bytes(bytes).ok(),
            Value::IntValue(_) => None,
        })
    }

    fn set_switching_set_assignment(&mut self, assignment: &SwitchingSetAssignment) {
        let key = ParameterType::SwitchingSetAssignment as u64;
        self.set_bytesvalue(key, assignment.to_bytes());
    }

    fn has_switching_set_assignment(&self) -> bool {
        let key = ParameterType::SwitchingSetAssignment as u64;
        self.has(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn encode_decode_basic() {
        let assignment = SwitchingSetAssignment::new(1, 3000)
            .with_fraction(6)
            .with_activate(true);

        let mut buf = BytesMut::new();
        assignment.encode(&mut buf).unwrap();

        let decoded = SwitchingSetAssignment::decode(&mut buf).unwrap();
        assert_eq!(decoded, assignment);
        assert_eq!(decoded.set_id, 1);
        assert_eq!(decoded.throughput_kbps, 3000);
        assert_eq!(decoded.fraction, 6);
        assert!(decoded.activate);
        assert_eq!(decoded.rank, None);
    }

    #[test]
    fn encode_decode_with_rank() {
        let assignment = SwitchingSetAssignment::new(2, 1500)
            .with_fraction(4)
            .with_rank(2)
            .with_activate(false);

        let mut buf = BytesMut::new();
        assignment.encode(&mut buf).unwrap();

        let decoded = SwitchingSetAssignment::decode(&mut buf).unwrap();
        assert_eq!(decoded, assignment);
        assert_eq!(decoded.rank, Some(2));
        assert!(!decoded.activate);
    }

    #[test]
    fn encode_decode_via_bytes() {
        let assignment = SwitchingSetAssignment::new(3, 800)
            .with_fraction(2)
            .with_rank(1)
            .with_activate(true);

        let bytes = assignment.to_bytes();
        let decoded = SwitchingSetAssignment::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, assignment);
    }

    #[test]
    fn kvp_integration() {
        let mut params = KeyValuePairs::new();

        let assignment = SwitchingSetAssignment::new(1, 3000)
            .with_fraction(6)
            .with_rank(1)
            .with_activate(true);

        params.set_switching_set_assignment(&assignment);

        assert!(params.has_switching_set_assignment());

        let retrieved = params.get_switching_set_assignment().unwrap();
        assert_eq!(retrieved, assignment);
    }

    #[test]
    fn kvp_roundtrip() {
        let mut params = KeyValuePairs::new();

        let assignment = SwitchingSetAssignment::new(5, 2000)
            .with_fraction(8)
            .with_activate(true);

        params.set_switching_set_assignment(&assignment);

        // Encode and decode the entire KeyValuePairs
        let mut buf = BytesMut::new();
        params.encode(&mut buf).unwrap();

        let decoded_params = KeyValuePairs::decode(&mut buf).unwrap();
        let decoded_assignment = decoded_params.get_switching_set_assignment().unwrap();
        assert_eq!(decoded_assignment, assignment);
    }

    #[test]
    fn rank_zero_is_invalid() {
        let assignment = SwitchingSetAssignment {
            set_id: 1,
            throughput_kbps: 1000,
            fraction: 5,
            activate: false,
            rank: Some(0),
        };

        let mut buf = BytesMut::new();
        let result = assignment.encode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn effective_rank_defaults_to_one() {
        let assignment = SwitchingSetAssignment::new(1, 1000);
        assert_eq!(assignment.effective_rank(), 1);

        let assignment_with_rank = assignment.with_rank(5);
        assert_eq!(assignment_with_rank.effective_rank(), 5);
    }

    #[test]
    fn multiple_assignments_different_tracks() {
        // Simulate ABR scenario: same set, different throughput thresholds
        let high = SwitchingSetAssignment::new(1, 3000)
            .with_fraction(6)
            .with_activate(false);

        let medium = SwitchingSetAssignment::new(1, 1500)
            .with_fraction(6)
            .with_activate(false);

        let low = SwitchingSetAssignment::new(1, 800)
            .with_fraction(6)
            .with_activate(true); // Last one activates

        // Each would go in a different SUBSCRIBE message
        let mut params_high = KeyValuePairs::new();
        params_high.set_switching_set_assignment(&high);

        let mut params_medium = KeyValuePairs::new();
        params_medium.set_switching_set_assignment(&medium);

        let mut params_low = KeyValuePairs::new();
        params_low.set_switching_set_assignment(&low);

        // Verify they're independent
        assert!(!params_high.get_switching_set_assignment().unwrap().activate);
        assert!(!params_medium.get_switching_set_assignment().unwrap().activate);
        assert!(params_low.get_switching_set_assignment().unwrap().activate);
    }
}
