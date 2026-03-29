use crate::coding::{Decode, DecodeError, Encode, EncodeError, KeyValuePairs, TrackNamespace};
use crate::message::{ParameterType, TrackFilter};

/// Subscribe Namespace
///
/// Per PR #1518, SUBSCRIBE_NAMESPACE can include a TRACK_FILTER parameter (0x29)
/// for top-N track selection based on property values.
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

    /// Optional track filter for top-N selection (parsed from params)
    #[doc(hidden)]
    track_filter_cache: Option<TrackFilter>,
}

impl SubscribeNamespace {
    /// Creates a new SubscribeNamespace message.
    pub fn new(id: u64, track_namespace_prefix: TrackNamespace, forward: u8) -> Self {
        Self {
            id,
            track_namespace_prefix,
            forward,
            params: KeyValuePairs::new(),
            track_filter_cache: None,
        }
    }

    /// Sets a track filter for this namespace subscription.
    ///
    /// The track filter enables top-N selection based on property values,
    /// useful for scenarios like active speaker selection.
    pub fn set_track_filter(&mut self, filter: TrackFilter) {
        // Encode the filter into params
        let mut buf = Vec::new();
        if filter.encode(&mut buf).is_ok() {
            self.params
                .set_bytesvalue(ParameterType::TrackFilter as u64 | 1, buf);
            self.track_filter_cache = Some(filter);
        }
    }

    /// Returns the track filter if one is configured.
    pub fn track_filter(&self) -> Option<&TrackFilter> {
        self.track_filter_cache.as_ref()
    }

    /// Parses and returns the track filter from params.
    ///
    /// Call this after decoding to populate the cache.
    pub fn parse_track_filter(&mut self) -> Option<TrackFilter> {
        // Track filter uses odd key (bytes value)
        let key = ParameterType::TrackFilter as u64 | 1;
        if let Some(bytes) = self.params.get_bytesvalue(key) {
            let mut buf = bytes::Bytes::from(bytes.clone());
            if let Ok(filter) = TrackFilter::decode(&mut buf) {
                self.track_filter_cache = Some(filter.clone());
                return Some(filter);
            }
        }
        None
    }
}

impl Decode for SubscribeNamespace {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let id = u64::decode(r)?;
        let track_namespace_prefix = TrackNamespace::decode(r)?;
        let forward = u8::decode(r)?;
        let params = KeyValuePairs::decode(r)?;

        let mut msg = Self {
            id,
            track_namespace_prefix,
            forward,
            params,
            track_filter_cache: None,
        };

        // Try to parse track filter from params
        msg.parse_track_filter();

        Ok(msg)
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
            track_filter_cache: None,
        };
        msg.encode(&mut buf).unwrap();
        let decoded = SubscribeNamespace::decode(&mut buf).unwrap();
        assert_eq!(decoded.id, msg.id);
        assert_eq!(decoded.forward, msg.forward);
        assert_eq!(decoded.track_namespace_prefix, msg.track_namespace_prefix);
    }

    #[test]
    fn encode_decode_with_track_filter() {
        let mut buf = BytesMut::new();

        let mut msg = SubscribeNamespace::new(
            42,
            TrackNamespace::from_utf8_path("conference/room1"),
            1,
        );

        // Set a track filter for active speaker selection
        let filter = TrackFilter::new(0x100, 3, 2000).unwrap();
        msg.set_track_filter(filter.clone());

        msg.encode(&mut buf).unwrap();
        let decoded = SubscribeNamespace::decode(&mut buf).unwrap();

        assert_eq!(decoded.id, 42);
        assert!(decoded.track_filter().is_some());
        let decoded_filter = decoded.track_filter().unwrap();
        assert_eq!(decoded_filter.property_type, 0x100);
        assert_eq!(decoded_filter.max_tracks_selected, 3);
        assert_eq!(decoded_filter.timeout_ms, 2000);
    }
}
