use crate::coding::{Decode, DecodeError, Encode, EncodeError, TrackNamespace};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsubscribeNamespace {
    pub track_namespace_prefix: TrackNamespace,
}

impl Decode for UnsubscribeNamespace {
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let track_namespace_prefix = TrackNamespace::decode(r)?;
        Ok(Self {
            track_namespace_prefix,
        })
    }
}

impl Encode for UnsubscribeNamespace {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        self.track_namespace_prefix.encode(w)?;
        Ok(())
    }
}
