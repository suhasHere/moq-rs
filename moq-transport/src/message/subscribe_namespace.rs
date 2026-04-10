use crate::coding::{Decode, DecodeError, Encode, EncodeError, KeyValuePairs, TrackNamespace};
use crate::message::TrackFilter;

/// Subscribe Namespace
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribeNamespace {
    /// The subscription request ID
    pub id: u64,

    /// The track namespace prefix
    pub track_namespace_prefix: TrackNamespace,

    /// The Forward value that new subscriptions resulting from this SUBSCRIBE_NAMESPACE will have
    pub forward: u8,

    /// Optional parameters
    pub params: KeyValuePairs,
}

impl SubscribeNamespace {
    /// Creates a new SubscribeNamespace message.
    pub fn new(id: u64, track_namespace_prefix: TrackNamespace, forward: u8) -> Self {
        Self {
            id,
            track_namespace_prefix,
            forward,
            params: KeyValuePairs::new(),
        }
    }

    /// Sets the track filter parameter.
    pub fn set_track_filter(&mut self, filter: TrackFilter) {
        // Encode the track filter and store as a parameter
        let mut buf = Vec::new();
        filter.encode(&mut buf).expect("TrackFilter encoding should not fail");
        // Use parameter type 0x03 for TRACK_FILTER (as defined in draft-16)
        self.params.set_bytesvalue(0x03, buf);
    }

    /// Gets the track filter parameter if present.
    pub fn track_filter(&self) -> Option<TrackFilter> {
        self.params.get_bytesvalue(0x03).and_then(|bytes| {
            let mut buf = std::io::Cursor::new(bytes);
            TrackFilter::decode(&mut buf).ok()
        })
    }
}

impl Decode for SubscribeNamespace {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let id = u64::decode(r)?;
        let track_namespace_prefix = TrackNamespace::decode(r)?;
        let forward = u8::decode(r)?;
        let params = KeyValuePairs::decode(r)?;

        Ok(Self {
            id,
            track_namespace_prefix,
            forward,
            params,
        })
    }
}

impl Encode for SubscribeNamespace {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.id.encode(w)?;
        self.track_namespace_prefix.encode(w)?;
        self.forward.encode(w)?;
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

        // One parameter for testing
        let mut kvps = KeyValuePairs::new();
        kvps.set_bytesvalue(123, vec![0x00, 0x01, 0x02, 0x03]);

        let msg = SubscribeNamespace {
            id: 12345,
            forward: 0,
            track_namespace_prefix: TrackNamespace::from_utf8_path("path/prefix"),
            params: kvps,
        };
        msg.encode(&mut buf).unwrap();
        let decoded = SubscribeNamespace::decode(&mut buf).unwrap();
        assert_eq!(decoded.id, msg.id);
        assert_eq!(decoded.forward, msg.forward);
        assert_eq!(decoded.track_namespace_prefix, msg.track_namespace_prefix);
    }
}
