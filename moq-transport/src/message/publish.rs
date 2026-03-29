use crate::coding::{Decode, DecodeError, Encode, EncodeError, KeyValuePairs, TrackNamespace};

/// Sent by publisher to initiate a subscription to a track.
///
/// Draft-16: Fields like group_order, content_exists, largest_location, forward
/// have been moved to Parameters (Section 9.2.2).
/// Note: Draft-16 PUBLISH does NOT include track_extensions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publish {
    /// The publish request ID
    pub id: u64,

    /// Track properties
    pub track_namespace: TrackNamespace,
    pub track_name: String, // TODO SLG - consider making a FullTrackName base struct (total size limit of 4096)
    pub track_alias: u64,

    /// Optional parameters (may contain Forward, GroupOrder, LargestObject, PublisherPriority, etc.)
    pub params: KeyValuePairs,
}

impl Decode for Publish {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let id = u64::decode(r)?;

        let track_namespace = TrackNamespace::decode(r)?;
        let track_name = String::decode(r)?;
        let track_alias = u64::decode(r)?;

        let params = KeyValuePairs::decode(r)?;

        Ok(Self {
            id,
            track_namespace,
            track_name,
            track_alias,
            params,
        })
    }
}

impl Encode for Publish {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.id.encode(w)?;

        self.track_namespace.encode(w)?;
        self.track_name.encode(w)?;
        self.track_alias.encode(w)?;

        self.params.encode(w)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn encode_decode() {
        let mut buf = BytesMut::new();

        let mut kvps = KeyValuePairs::new();
        kvps.set_bytesvalue(123, vec![0x00, 0x01, 0x02, 0x03]);

        let msg = Publish {
            id: 12345,
            track_namespace: TrackNamespace::from_utf8_path("test/path/to/resource"),
            track_name: "audiotrack".to_string(),
            track_alias: 212,
            params: kvps.clone(),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = Publish::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn encode_decode_no_params() {
        let mut buf = BytesMut::new();

        let msg = Publish {
            id: 12345,
            track_namespace: TrackNamespace::from_utf8_path("test/path/to/resource"),
            track_name: "audiotrack".to_string(),
            track_alias: 212,
            params: Default::default(),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = Publish::decode(&mut buf).unwrap();
        assert_eq!(decoded, msg);
    }
}
