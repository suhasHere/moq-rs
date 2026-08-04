use crate::coding::{Decode, DecodeError, Encode, EncodeError};
use crate::data::{ExtensionHeaders, ObjectStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatagramType {
    // Payload types with Priority Present (0x00-0x07)
    ObjectIdPayload = 0x00,
    ObjectIdPayloadExt = 0x01,
    ObjectIdPayloadEndOfGroup = 0x02,
    ObjectIdPayloadExtEndOfGroup = 0x03,
    Payload = 0x04,
    PayloadExt = 0x05,
    PayloadEndOfGroup = 0x06,
    PayloadExtEndOfGroup = 0x07,
    // Payload types with Priority Not Present (0x08-0x0F)
    ObjectIdPayloadNoPriority = 0x08,
    ObjectIdPayloadExtNoPriority = 0x09,
    ObjectIdPayloadEndOfGroupNoPriority = 0x0a,
    ObjectIdPayloadExtEndOfGroupNoPriority = 0x0b,
    PayloadNoPriority = 0x0c,
    PayloadExtNoPriority = 0x0d,
    PayloadEndOfGroupNoPriority = 0x0e,
    PayloadExtEndOfGroupNoPriority = 0x0f,
    // Status types with Priority Present (0x20-0x25)
    ObjectIdStatus = 0x20,
    ObjectIdStatusExt = 0x21,
    Status = 0x24,
    StatusExt = 0x25,
    // Status types with Priority Not Present (0x28-0x2D)
    ObjectIdStatusNoPriority = 0x28,
    ObjectIdStatusExtNoPriority = 0x29,
    StatusNoPriority = 0x2c,
    StatusExtNoPriority = 0x2d,
}

impl DatagramType {
    /// Returns true if this datagram type has the Object ID field present
    pub fn has_object_id(&self) -> bool {
        matches!(
            *self,
            DatagramType::ObjectIdPayload
                | DatagramType::ObjectIdPayloadExt
                | DatagramType::ObjectIdPayloadEndOfGroup
                | DatagramType::ObjectIdPayloadExtEndOfGroup
                | DatagramType::ObjectIdPayloadNoPriority
                | DatagramType::ObjectIdPayloadExtNoPriority
                | DatagramType::ObjectIdPayloadEndOfGroupNoPriority
                | DatagramType::ObjectIdPayloadExtEndOfGroupNoPriority
                | DatagramType::ObjectIdStatus
                | DatagramType::ObjectIdStatusExt
                | DatagramType::ObjectIdStatusNoPriority
                | DatagramType::ObjectIdStatusExtNoPriority
        )
    }

    /// Returns true if this datagram type has the Publisher Priority field present
    pub fn has_priority(&self) -> bool {
        matches!(
            *self,
            DatagramType::ObjectIdPayload
                | DatagramType::ObjectIdPayloadExt
                | DatagramType::ObjectIdPayloadEndOfGroup
                | DatagramType::ObjectIdPayloadExtEndOfGroup
                | DatagramType::Payload
                | DatagramType::PayloadExt
                | DatagramType::PayloadEndOfGroup
                | DatagramType::PayloadExtEndOfGroup
                | DatagramType::ObjectIdStatus
                | DatagramType::ObjectIdStatusExt
                | DatagramType::Status
                | DatagramType::StatusExt
        )
    }

    /// Returns true if this datagram type has extension headers
    pub fn has_extensions(&self) -> bool {
        matches!(
            *self,
            DatagramType::ObjectIdPayloadExt
                | DatagramType::ObjectIdPayloadExtEndOfGroup
                | DatagramType::PayloadExt
                | DatagramType::PayloadExtEndOfGroup
                | DatagramType::ObjectIdPayloadExtNoPriority
                | DatagramType::ObjectIdPayloadExtEndOfGroupNoPriority
                | DatagramType::PayloadExtNoPriority
                | DatagramType::PayloadExtEndOfGroupNoPriority
                | DatagramType::ObjectIdStatusExt
                | DatagramType::StatusExt
                | DatagramType::ObjectIdStatusExtNoPriority
                | DatagramType::StatusExtNoPriority
        )
    }

    /// Returns true if this is a status datagram (no payload)
    pub fn is_status(&self) -> bool {
        matches!(
            *self,
            DatagramType::ObjectIdStatus
                | DatagramType::ObjectIdStatusExt
                | DatagramType::Status
                | DatagramType::StatusExt
                | DatagramType::ObjectIdStatusNoPriority
                | DatagramType::ObjectIdStatusExtNoPriority
                | DatagramType::StatusNoPriority
                | DatagramType::StatusExtNoPriority
        )
    }

    /// Returns true if this is a payload datagram
    pub fn is_payload(&self) -> bool {
        !self.is_status()
    }

    /// Returns true if this datagram type indicates end of group
    pub fn is_end_of_group(&self) -> bool {
        matches!(
            *self,
            DatagramType::ObjectIdPayloadEndOfGroup
                | DatagramType::ObjectIdPayloadExtEndOfGroup
                | DatagramType::PayloadEndOfGroup
                | DatagramType::PayloadExtEndOfGroup
                | DatagramType::ObjectIdPayloadEndOfGroupNoPriority
                | DatagramType::ObjectIdPayloadExtEndOfGroupNoPriority
                | DatagramType::PayloadEndOfGroupNoPriority
                | DatagramType::PayloadExtEndOfGroupNoPriority
        )
    }
}

impl Decode for DatagramType {
    fn decode<B: bytes::Buf>(r: &mut B) -> Result<Self, DecodeError> {
        match u64::decode(r)? {
            // Payload types with Priority Present (0x00-0x07)
            0x00 => Ok(Self::ObjectIdPayload),
            0x01 => Ok(Self::ObjectIdPayloadExt),
            0x02 => Ok(Self::ObjectIdPayloadEndOfGroup),
            0x03 => Ok(Self::ObjectIdPayloadExtEndOfGroup),
            0x04 => Ok(Self::Payload),
            0x05 => Ok(Self::PayloadExt),
            0x06 => Ok(Self::PayloadEndOfGroup),
            0x07 => Ok(Self::PayloadExtEndOfGroup),
            // Payload types with Priority Not Present (0x08-0x0F)
            0x08 => Ok(Self::ObjectIdPayloadNoPriority),
            0x09 => Ok(Self::ObjectIdPayloadExtNoPriority),
            0x0a => Ok(Self::ObjectIdPayloadEndOfGroupNoPriority),
            0x0b => Ok(Self::ObjectIdPayloadExtEndOfGroupNoPriority),
            0x0c => Ok(Self::PayloadNoPriority),
            0x0d => Ok(Self::PayloadExtNoPriority),
            0x0e => Ok(Self::PayloadEndOfGroupNoPriority),
            0x0f => Ok(Self::PayloadExtEndOfGroupNoPriority),
            // Status types with Priority Present (0x20-0x25)
            0x20 => Ok(Self::ObjectIdStatus),
            0x21 => Ok(Self::ObjectIdStatusExt),
            0x24 => Ok(Self::Status),
            0x25 => Ok(Self::StatusExt),
            // Status types with Priority Not Present (0x28-0x2D)
            0x28 => Ok(Self::ObjectIdStatusNoPriority),
            0x29 => Ok(Self::ObjectIdStatusExtNoPriority),
            0x2c => Ok(Self::StatusNoPriority),
            0x2d => Ok(Self::StatusExtNoPriority),
            _ => Err(DecodeError::InvalidDatagramType),
        }
    }
}

impl Encode for DatagramType {
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        let val = *self as u64;
        val.encode(w)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Datagram {
    /// The type of this datagram object
    pub datagram_type: DatagramType,

    /// The track alias.
    pub track_alias: u64,

    /// The sequence number within the track.
    pub group_id: u64,

    /// The object ID within the group.
    pub object_id: Option<u64>,

    /// Publisher priority, where **smaller** values are sent first.
    /// Optional when using NoPriority datagram types (0x08-0x0F, 0x28-0x2D).
    pub publisher_priority: Option<u8>,

    /// Optional extension headers for types with extensions
    pub extension_headers: Option<ExtensionHeaders>,

    /// The Object Status.
    pub status: Option<ObjectStatus>,

    /// The payload.
    pub payload: Option<bytes::Bytes>,
}

impl Decode for Datagram {
    /// Decode a datagram using draft-16 wire format:
    /// Type(0x01) | TrackAlias | GroupID | ObjectID | ExtensionHeaders(length-prefixed) | Payload
    fn decode<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
        let type_val = u64::decode(r)?;

        if type_val != 0x01 {
            return Err(DecodeError::InvalidDatagramType);
        }

        let track_alias = u64::decode(r)?;
        let group_id = u64::decode(r)?;
        let object_id = u64::decode(r)?;

        let extension_headers = ExtensionHeaders::decode(r)?;
        let ext = if extension_headers.is_empty() {
            None
        } else {
            Some(extension_headers)
        };

        let payload = if r.has_remaining() {
            Some(r.copy_to_bytes(r.remaining()))
        } else {
            None
        };

        Ok(Self {
            datagram_type: DatagramType::ObjectIdPayload,
            track_alias,
            group_id,
            object_id: Some(object_id),
            publisher_priority: None,
            extension_headers: ext,
            status: None,
            payload,
        })
    }
}

impl Encode for Datagram {
    /// Encode a datagram using draft-16 wire format:
    /// Type(0x01) | TrackAlias | GroupID | ObjectID | ExtensionHeaders(length-prefixed) | Payload
    fn encode<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
        // Always emit draft-16 DataStreamType 0x01
        (0x01u64).encode(w)?;
        self.track_alias.encode(w)?;
        self.group_id.encode(w)?;

        if let Some(object_id) = self.object_id {
            object_id.encode(w)?;
        } else {
            return Err(EncodeError::MissingField("ObjectId".to_string()));
        }

        if let Some(ref ext) = self.extension_headers {
            ext.encode(w)?;
        } else {
            (0u64).encode(w)?;
        }

        if let Some(ref payload) = self.payload {
            w.put_slice(payload);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use bytes::BytesMut;

    #[test]
    fn encode_decode_draft16_datagram_no_extensions() {
        let mut buf = BytesMut::new();

        let msg = Datagram {
            datagram_type: DatagramType::ObjectIdPayload,
            track_alias: 12,
            group_id: 10,
            object_id: Some(1234),
            publisher_priority: None,
            extension_headers: None,
            status: None,
            payload: Some(Bytes::from("payload")),
        };
        msg.encode(&mut buf).unwrap();
        // Type(1) + Alias(1) + GroupId(1) + ObjectId(2) + ExtLen(1=0) + Payload(7) = 13
        assert_eq!(13, buf.len());
        let decoded = Datagram::decode(&mut buf).unwrap();
        assert_eq!(decoded.track_alias, 12);
        assert_eq!(decoded.group_id, 10);
        assert_eq!(decoded.object_id, Some(1234));
        assert_eq!(decoded.extension_headers, None);
        assert_eq!(decoded.payload, Some(Bytes::from("payload")));
    }

    #[test]
    fn encode_decode_draft16_datagram_with_extensions() {
        let mut buf = BytesMut::new();

        let mut ext_hdrs = ExtensionHeaders::new();
        ext_hdrs.set_bytesvalue(123, vec![0x00, 0x01, 0x02, 0x03]);

        let msg = Datagram {
            datagram_type: DatagramType::ObjectIdPayload,
            track_alias: 12,
            group_id: 10,
            object_id: Some(1234),
            publisher_priority: None,
            extension_headers: Some(ext_hdrs.clone()),
            status: None,
            payload: Some(Bytes::from("payload")),
        };
        msg.encode(&mut buf).unwrap();
        let decoded = Datagram::decode(&mut buf).unwrap();
        assert_eq!(decoded.track_alias, 12);
        assert_eq!(decoded.group_id, 10);
        assert_eq!(decoded.object_id, Some(1234));
        assert_eq!(decoded.extension_headers, Some(ext_hdrs));
        assert_eq!(decoded.payload, Some(Bytes::from("payload")));
    }

    #[test]
    fn encode_decode_draft16_datagram_no_payload() {
        let mut buf = BytesMut::new();

        let msg = Datagram {
            datagram_type: DatagramType::ObjectIdPayload,
            track_alias: 5,
            group_id: 0,
            object_id: Some(0),
            publisher_priority: None,
            extension_headers: None,
            status: None,
            payload: None,
        };
        msg.encode(&mut buf).unwrap();
        let decoded = Datagram::decode(&mut buf).unwrap();
        assert_eq!(decoded.track_alias, 5);
        assert_eq!(decoded.group_id, 0);
        assert_eq!(decoded.object_id, Some(0));
        assert_eq!(decoded.payload, None);
    }

    #[test]
    fn encode_missing_object_id_fails() {
        let mut buf = BytesMut::new();

        let msg = Datagram {
            datagram_type: DatagramType::ObjectIdPayload,
            track_alias: 12,
            group_id: 10,
            object_id: None,
            publisher_priority: None,
            extension_headers: None,
            status: None,
            payload: Some(Bytes::from("payload")),
        };
        let result = msg.encode(&mut buf);
        assert!(matches!(result.unwrap_err(), EncodeError::MissingField(_)));
    }

    #[test]
    fn decode_invalid_type_fails() {
        let mut buf = BytesMut::new();
        (0x05u64).encode(&mut buf).unwrap();
        (12u64).encode(&mut buf).unwrap();
        let result = Datagram::decode(&mut buf);
        assert!(matches!(result.unwrap_err(), DecodeError::InvalidDatagramType));
    }
}
