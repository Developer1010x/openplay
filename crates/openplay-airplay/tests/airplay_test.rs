use openplay_airplay::{
    features::AirPlayFeatures,
    mirror_header::{MirrorHeader, PacketType, HEADER_SIZE},
    ntp::ntp_timestamp_now,
    tlv8::{self, Tlv8Item},
};

// ── AirPlayFeatures ────────────────────────────────────────────────────────────

#[test]
fn features_zero_supports_nothing() {
    let f = AirPlayFeatures::parse("0x0").unwrap();
    assert!(!f.supports_mirroring());
    assert!(!f.supports_video());
    assert!(!f.supports_audio());
    assert!(!f.requires_hk_pairing());
}

#[test]
fn features_raw_preserves_value() {
    let f = AirPlayFeatures::parse("0x1A").unwrap();
    assert_eq!(f.raw() & 0x1A, 0x1A);
}

#[test]
fn features_max_u64_parses() {
    let s = format!("0x{:X}", u32::MAX);
    assert!(AirPlayFeatures::parse(&s).is_ok());
}

#[test]
fn features_invalid_hex_returns_none() {
    assert!(AirPlayFeatures::parse("not_hex").is_none());
    assert!(AirPlayFeatures::parse("").is_none());
}

#[test]
fn features_split_format_combines_lo_hi() {
    let lo: u64 = 0x5A7FFFF7;
    let hi: u64 = 0x1E;
    let s = format!("0x{:X},0x{:X}", lo, hi);
    let f = AirPlayFeatures::parse(&s).unwrap();
    let expected = lo | (hi << 32);
    assert_eq!(f.raw(), expected);
}

// ── NTP ───────────────────────────────────────────────────────────────────────

#[test]
fn ntp_timestamp_is_nonzero() {
    assert!(ntp_timestamp_now() > 0);
}

#[test]
fn ntp_timestamp_increases() {
    let a = ntp_timestamp_now();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let b = ntp_timestamp_now();
    assert!(b >= a, "NTP timestamp must be non-decreasing");
}

#[test]
fn ntp_timestamp_in_ntp_epoch() {
    // NTP epoch starts Jan 1 1900. By 2024 we should have > 3.9e9 seconds elapsed.
    // In NTP fixed-point (upper 32 bits = seconds), value >> 32 gives seconds.
    let ts = ntp_timestamp_now();
    let seconds = ts >> 32;
    assert!(seconds > 3_900_000_000, "Expected modern NTP seconds, got {seconds}");
}

// ── TLV8 ─────────────────────────────────────────────────────────────────────

#[test]
fn encode_decode_single_item() {
    let items = vec![tlv8::item(0x01, b"hello")];
    let encoded = tlv8::encode(&items);
    let decoded = tlv8::decode(&encoded).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].tag, 0x01);
    assert_eq!(decoded[0].value, b"hello");
}

#[test]
fn encode_decode_multiple_items() {
    let items = vec![
        tlv8::item(0x01, b"first"),
        tlv8::item(0x02, b"second"),
        tlv8::item(0x03, b"third"),
    ];
    let encoded = tlv8::encode(&items);
    let decoded = tlv8::decode(&encoded).unwrap();
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[0].value, b"first");
    assert_eq!(decoded[1].value, b"second");
    assert_eq!(decoded[2].value, b"third");
}

#[test]
fn empty_value_item() {
    let items = vec![tlv8::item(0x05, b"")];
    let encoded = tlv8::encode(&items);
    let decoded = tlv8::decode(&encoded).unwrap();
    assert_eq!(decoded[0].tag, 0x05);
    assert!(decoded[0].value.is_empty());
}

#[test]
fn item_u8_helper() {
    let item = tlv8::item_u8(0x06, 42);
    assert_eq!(item.tag, 0x06);
    assert_eq!(item.value, vec![42u8]);
}

#[test]
fn lookup_finds_item_by_tag() {
    let items = vec![
        tlv8::item(0x01, b"one"),
        tlv8::item(0x02, b"two"),
    ];
    assert_eq!(tlv8::lookup(&items, 0x01), Some(b"one".as_ref()));
    assert_eq!(tlv8::lookup(&items, 0x02), Some(b"two".as_ref()));
    assert_eq!(tlv8::lookup(&items, 0x99), None);
}

#[test]
fn fragmentation_of_255_byte_value() {
    let data = vec![0xABu8; 255];
    let items = vec![tlv8::item(0x07, &data)];
    let encoded = tlv8::encode(&items);
    let decoded = tlv8::decode(&encoded).unwrap();
    assert_eq!(decoded[0].value, data);
}

#[test]
fn fragmentation_of_256_byte_value() {
    let data = vec![0xCDu8; 256];
    let items = vec![tlv8::item(0x08, &data)];
    let encoded = tlv8::encode(&items);
    let decoded = tlv8::decode(&encoded).unwrap();
    assert_eq!(decoded[0].value, data);
}

#[test]
fn fragmentation_of_large_value() {
    let data = vec![0xEFu8; 600];
    let items = vec![tlv8::item(0x09, &data)];
    let encoded = tlv8::encode(&items);
    let decoded = tlv8::decode(&encoded).unwrap();
    assert_eq!(decoded[0].value, data);
}

#[test]
fn to_map_indexes_by_tag() {
    let items = vec![
        tlv8::item(0x01, b"alpha"),
        tlv8::item(0x02, b"beta"),
    ];
    let map = tlv8::to_map(&items);
    assert_eq!(map[&0x01], b"alpha");
    assert_eq!(map[&0x02], b"beta");
}

#[test]
fn decode_truncated_header_errors() {
    let bad = vec![0x01u8]; // tag without length
    assert!(tlv8::decode(&bad).is_err());
}

#[test]
fn decode_truncated_value_errors() {
    let bad = vec![0x01u8, 0x05, 0xAA, 0xBB]; // says 5 bytes but only 2 follow
    assert!(tlv8::decode(&bad).is_err());
}

#[test]
fn decode_empty_input_returns_empty_vec() {
    let decoded = tlv8::decode(&[]).unwrap();
    assert!(decoded.is_empty());
}

// ── MirrorHeader ──────────────────────────────────────────────────────────────

#[test]
fn header_size_is_128() {
    assert_eq!(HEADER_SIZE, 128);
}

#[test]
fn video_packet_roundtrip() {
    let h = MirrorHeader {
        payload_size: 12345,
        packet_type: PacketType::Video,
        ntp_timestamp: 999_888_777,
    };
    let encoded = h.encode();
    assert_eq!(encoded.len(), HEADER_SIZE);
    let decoded = MirrorHeader::decode(&encoded).unwrap();
    assert_eq!(decoded.payload_size, 12345);
    assert_eq!(decoded.ntp_timestamp, 999_888_777);
    assert!(matches!(decoded.packet_type, PacketType::Video));
}

#[test]
fn codec_data_packet_roundtrip() {
    let h = MirrorHeader {
        payload_size: 32,
        packet_type: PacketType::CodecData,
        ntp_timestamp: 1,
    };
    let encoded = h.encode();
    let decoded = MirrorHeader::decode(&encoded).unwrap();
    assert!(matches!(decoded.packet_type, PacketType::CodecData));
}

#[test]
fn heartbeat_packet_roundtrip() {
    let h = MirrorHeader {
        payload_size: 0,
        packet_type: PacketType::Heartbeat,
        ntp_timestamp: ntp_timestamp_now(),
    };
    let encoded = h.encode();
    let decoded = MirrorHeader::decode(&encoded).unwrap();
    assert!(matches!(decoded.packet_type, PacketType::Heartbeat));
    assert_eq!(decoded.payload_size, 0);
}

#[test]
fn payload_size_uses_little_endian() {
    // 0x01020304 in LE = bytes [04, 03, 02, 01] at offset 0
    let h = MirrorHeader {
        payload_size: 0x01020304,
        packet_type: PacketType::Video,
        ntp_timestamp: 0,
    };
    let encoded = h.encode();
    assert_eq!(encoded[0], 0x04);
    assert_eq!(encoded[1], 0x03);
    assert_eq!(encoded[2], 0x02);
    assert_eq!(encoded[3], 0x01);
}

#[test]
fn decode_too_short_returns_none() {
    let short = vec![0u8; 64];
    assert!(MirrorHeader::decode(&short).is_none());
}

#[test]
fn decode_max_payload_size() {
    let h = MirrorHeader {
        payload_size: u32::MAX,
        packet_type: PacketType::Video,
        ntp_timestamp: u64::MAX,
    };
    let encoded = h.encode();
    let decoded = MirrorHeader::decode(&encoded).unwrap();
    assert_eq!(decoded.payload_size, u32::MAX);
    assert_eq!(decoded.ntp_timestamp, u64::MAX);
}
