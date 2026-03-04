use bytes::{BufMut, BytesMut};

/// AirPlay mirror packet types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum PacketType {
    /// H.264 video frame data.
    Video = 0,
    /// Codec configuration data (SPS/PPS).
    CodecData = 1,
    /// Heartbeat / keep-alive.
    Heartbeat = 2,
}

impl PacketType {
    pub fn from_u16(val: u16) -> Option<Self> {
        match val {
            0 => Some(Self::Video),
            1 => Some(Self::CodecData),
            2 => Some(Self::Heartbeat),
            _ => None,
        }
    }
}

/// 128-byte AirPlay mirror packet header.
///
/// This header prefixes every packet on the AirPlay mirror TCP stream.
/// The layout is based on reverse-engineered AirPlay protocol:
///
/// ```text
/// Offset  Size  Field
/// 0       4     payload_size (little-endian)
/// 4       2     packet_type (little-endian, 0=video, 1=codec_data, 2=heartbeat)
/// 6       2     padding
/// 8       8     ntp_timestamp (big-endian NTP timestamp)
/// 16      112   reserved (zeros)
/// ```
#[derive(Debug, Clone)]
pub struct MirrorHeader {
    pub payload_size: u32,
    pub packet_type: PacketType,
    pub ntp_timestamp: u64,
}

/// Total header size in bytes.
pub const HEADER_SIZE: usize = 128;

impl MirrorHeader {
    /// Creates a new mirror header.
    pub fn new(packet_type: PacketType, payload_size: u32, ntp_timestamp: u64) -> Self {
        Self {
            payload_size,
            packet_type,
            ntp_timestamp,
        }
    }

    /// Serializes the header into 128 bytes.
    pub fn encode(&self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(HEADER_SIZE);

        // payload_size (4 bytes, little-endian)
        buf.put_u32_le(self.payload_size);
        // packet_type (2 bytes, little-endian)
        buf.put_u16_le(self.packet_type as u16);
        // padding (2 bytes)
        buf.put_u16_le(0);
        // ntp_timestamp (8 bytes, big-endian)
        buf.put_u64(self.ntp_timestamp);
        // reserved (112 bytes of zeros)
        buf.put_bytes(0, HEADER_SIZE - 16);

        buf
    }

    /// Deserializes a header from a 128-byte buffer.
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < HEADER_SIZE {
            return None;
        }

        let payload_size = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let type_val = u16::from_le_bytes([data[4], data[5]]);
        let packet_type = PacketType::from_u16(type_val)?;
        let ntp_timestamp = u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);

        Some(Self {
            payload_size,
            packet_type,
            ntp_timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_roundtrip_video() {
        let header = MirrorHeader::new(PacketType::Video, 12345, 0xAABBCCDD00112233);
        let encoded = header.encode();
        assert_eq!(encoded.len(), HEADER_SIZE);

        let decoded = MirrorHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.payload_size, 12345);
        assert_eq!(decoded.packet_type, PacketType::Video);
        assert_eq!(decoded.ntp_timestamp, 0xAABBCCDD00112233);
    }

    #[test]
    fn test_header_roundtrip_codec_data() {
        let header = MirrorHeader::new(PacketType::CodecData, 64, 0);
        let encoded = header.encode();
        let decoded = MirrorHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::CodecData);
        assert_eq!(decoded.payload_size, 64);
    }

    #[test]
    fn test_header_roundtrip_heartbeat() {
        let header = MirrorHeader::new(PacketType::Heartbeat, 0, 999);
        let encoded = header.encode();
        let decoded = MirrorHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.packet_type, PacketType::Heartbeat);
        assert_eq!(decoded.payload_size, 0);
        assert_eq!(decoded.ntp_timestamp, 999);
    }

    #[test]
    fn test_header_size_constant() {
        let header = MirrorHeader::new(PacketType::Video, 0, 0);
        let encoded = header.encode();
        assert_eq!(encoded.len(), 128);
    }

    #[test]
    fn test_decode_too_short() {
        let data = [0u8; 64];
        assert!(MirrorHeader::decode(&data).is_none());
    }

    #[test]
    fn test_decode_invalid_type() {
        let mut data = [0u8; 128];
        data[4] = 0xFF; // invalid packet type
        data[5] = 0xFF;
        assert!(MirrorHeader::decode(&data).is_none());
    }

    #[test]
    fn test_payload_size_le_encoding() {
        let header = MirrorHeader::new(PacketType::Video, 0x04030201, 0);
        let encoded = header.encode();
        assert_eq!(encoded[0], 0x01);
        assert_eq!(encoded[1], 0x02);
        assert_eq!(encoded[2], 0x03);
        assert_eq!(encoded[3], 0x04);
    }
}
