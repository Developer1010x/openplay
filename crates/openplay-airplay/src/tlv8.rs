//! TLV8 encoding/decoding for HomeKit Accessory Protocol (HAP) pairing messages.
//!
//! TLV8 format: type(1B) + length(1B) + value(0-255B)
//! For values > 255 bytes, fragment into consecutive TLV items with the same type.

use std::collections::BTreeMap;

/// A single TLV8 item.
#[derive(Debug, Clone)]
pub struct Tlv8Item {
    pub tag: u8,
    pub value: Vec<u8>,
}

/// HAP TLV type constants.
///
/// Matches pyatv `pyatv/auth/hap_tlv8.py`'s `TlvValue` enum, which names
/// [`STATE`] `SeqNo` and [`RETRY_DELAY`] `BackOff`; the names here follow the
/// HAP specification's `kTLVType_*`, the values are the same.
pub mod tags {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    /// `kTLVType_RetryDelay`, in seconds. HAP integer TLVs are little-endian
    /// and variable width — see `hap_pairing::retry_delay_secs`.
    pub const RETRY_DELAY: u8 = 0x08;
    pub const CERTIFICATE: u8 = 0x09;
    pub const SIGNATURE: u8 = 0x0A;
    pub const PERMISSIONS: u8 = 0x0B;
    pub const FRAGMENT_DATA: u8 = 0x0C;
    pub const FRAGMENT_LAST: u8 = 0x0D;
    pub const FLAGS: u8 = 0x13;
    pub const SEPARATOR: u8 = 0xFF;
}

/// HAP pairing method constants (`kTLVType_Method` values).
///
/// Pinned by `method_constants_match_the_hap_specification` against pyatv
/// `pyatv/auth/hap_tlv8.py`'s `Method` enum. `PAIR_VERIFY` was `0x01` here until
/// that check was written; `0x01` is a *different real method*
/// (`PairSetupWithAuth`), so the wrong value named a method that exists and
/// would have been accepted and then answered for the wrong flow. It is kept
/// below, unused, so the collision stays visible.
pub mod methods {
    pub const PAIR_SETUP: u8 = 0x00;
    pub const PAIR_SETUP_WITH_AUTH: u8 = 0x01;
    pub const PAIR_VERIFY: u8 = 0x02;
    pub const ADD_PAIRING: u8 = 0x03;
    pub const REMOVE_PAIRING: u8 = 0x04;
    pub const LIST_PAIRINGS: u8 = 0x05;
}

/// HAP error codes (`kTLVType_Error` values).
pub mod errors {
    /// `kTLVError_None`. Not in pyatv's `ErrorCode` enum, which starts at
    /// `Unknown = 0x01`; the HAP specification defines 0x00 as "no error", and
    /// an accessory that sends the TLV with this value is reporting success.
    pub const NONE: u8 = 0x00;
    pub const UNKNOWN: u8 = 0x01;
    pub const AUTHENTICATION: u8 = 0x02;
    pub const BACKOFF: u8 = 0x03;
    pub const MAX_PEERS: u8 = 0x04;
    pub const MAX_TRIES: u8 = 0x05;
    pub const UNAVAILABLE: u8 = 0x06;
    pub const BUSY: u8 = 0x07;
}

/// Encode a list of TLV8 items into a byte buffer.
/// Values > 255 bytes are automatically fragmented.
pub fn encode(items: &[Tlv8Item]) -> Vec<u8> {
    let mut buf = Vec::new();
    for item in items {
        encode_item(&mut buf, item.tag, &item.value);
    }
    buf
}

fn encode_item(buf: &mut Vec<u8>, tag: u8, value: &[u8]) {
    if value.is_empty() {
        buf.push(tag);
        buf.push(0);
        return;
    }

    for chunk in value.chunks(255) {
        buf.push(tag);
        buf.push(chunk.len() as u8);
        buf.extend_from_slice(chunk);
    }
}

/// Decode a byte buffer into TLV8 items.
/// Consecutive items with the same tag are merged (defragmented).
pub fn decode(data: &[u8]) -> Result<Vec<Tlv8Item>, DecodeError> {
    let mut items: Vec<Tlv8Item> = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        if pos + 1 >= data.len() {
            return Err(DecodeError::TruncatedHeader(pos));
        }

        let tag = data[pos];
        let length = data[pos + 1] as usize;
        pos += 2;

        if pos + length > data.len() {
            return Err(DecodeError::TruncatedValue {
                offset: pos,
                expected: length,
                available: data.len() - pos,
            });
        }

        let value = data[pos..pos + length].to_vec();
        pos += length;

        // Merge with previous item if same tag (defragmentation)
        if let Some(last) = items.last_mut() {
            if last.tag == tag {
                last.value.extend_from_slice(&value);
                continue;
            }
        }

        items.push(Tlv8Item { tag, value });
    }

    Ok(items)
}

/// Look up a tag in decoded TLV items, returning its value.
pub fn lookup(items: &[Tlv8Item], tag: u8) -> Option<&[u8]> {
    items
        .iter()
        .find(|item| item.tag == tag)
        .map(|item| item.value.as_slice())
}

/// Convert decoded TLV items into a map for easy access.
pub fn to_map(items: &[Tlv8Item]) -> BTreeMap<u8, Vec<u8>> {
    items
        .iter()
        .map(|item| (item.tag, item.value.clone()))
        .collect()
}

/// Helper to build a TLV item.
pub fn item(tag: u8, value: impl Into<Vec<u8>>) -> Tlv8Item {
    Tlv8Item {
        tag,
        value: value.into(),
    }
}

/// Helper for a single-byte value.
pub fn item_u8(tag: u8, value: u8) -> Tlv8Item {
    Tlv8Item {
        tag,
        value: vec![value],
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Truncated TLV header at offset {0}")]
    TruncatedHeader(usize),

    #[error("Truncated TLV value at offset {offset}: expected {expected} bytes, got {available}")]
    TruncatedValue {
        offset: usize,
        expected: usize,
        available: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let items = vec![
            item_u8(tags::STATE, 1),
            item_u8(tags::METHOD, methods::PAIR_SETUP),
        ];

        let encoded = encode(&items);
        assert_eq!(encoded, &[0x06, 0x01, 0x01, 0x00, 0x01, 0x00]);

        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].tag, tags::STATE);
        assert_eq!(decoded[0].value, &[1]);
        assert_eq!(decoded[1].tag, tags::METHOD);
        assert_eq!(decoded[1].value, &[0]);
    }

    #[test]
    fn test_fragmentation() {
        let big_value = vec![0xAB; 300];
        let items = vec![Tlv8Item {
            tag: tags::PUBLIC_KEY,
            value: big_value.clone(),
        }];

        let encoded = encode(&items);

        // Should be fragmented: 255-byte chunk + 45-byte chunk
        assert_eq!(encoded[0], tags::PUBLIC_KEY);
        assert_eq!(encoded[1], 255);
        assert_eq!(encoded[2 + 255], tags::PUBLIC_KEY);
        assert_eq!(encoded[2 + 255 + 1], 45);

        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].tag, tags::PUBLIC_KEY);
        assert_eq!(decoded[0].value, big_value);
    }

    #[test]
    fn test_empty_value() {
        let items = vec![Tlv8Item {
            tag: tags::SEPARATOR,
            value: vec![],
        }];
        let encoded = encode(&items);
        assert_eq!(encoded, &[0xFF, 0x00]);

        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].tag, tags::SEPARATOR);
        assert!(decoded[0].value.is_empty());
    }

    #[test]
    fn test_lookup() {
        let items = vec![item_u8(tags::STATE, 2), item(tags::SALT, vec![1, 2, 3, 4])];

        assert_eq!(lookup(&items, tags::STATE), Some(&[2u8][..]));
        assert_eq!(lookup(&items, tags::SALT), Some(&[1, 2, 3, 4][..]));
        assert_eq!(lookup(&items, tags::PROOF), None);
    }

    #[test]
    fn test_truncated_errors() {
        assert!(decode(&[0x06]).is_err());
        assert!(decode(&[0x06, 0x05, 0x01]).is_err());
    }

    /// Pins every wire constant against an implementation known to interoperate
    /// with Apple hardware, rather than against our own reading of the spec.
    ///
    /// Reference: pyatv 0.18.0, `pyatv/auth/hap_tlv8.py` — `class Method`:
    ///
    /// ```text
    /// PairSetup = 0x00
    /// PairSetupWithAuth = 0x01
    /// PairVerify = 0x02
    /// AddPairing = 0x03
    /// RemovePairing = 0x04
    /// ListPairing = 0x05
    /// ```
    ///
    /// `PAIR_VERIFY` was `0x01` before this test existed. Nothing caught it,
    /// because no test asserted a value and pair-verify is unreachable until
    /// pair-setup is confirmed against hardware.
    #[test]
    fn method_constants_match_the_hap_specification() {
        assert_eq!(methods::PAIR_SETUP, 0x00);
        assert_eq!(methods::PAIR_SETUP_WITH_AUTH, 0x01);
        assert_eq!(methods::PAIR_VERIFY, 0x02, "0x01 is PairSetupWithAuth");
        assert_eq!(methods::ADD_PAIRING, 0x03);
        assert_eq!(methods::REMOVE_PAIRING, 0x04);
        assert_eq!(methods::LIST_PAIRINGS, 0x05);

        assert_ne!(
            methods::PAIR_VERIFY,
            methods::PAIR_SETUP_WITH_AUTH,
            "these are different methods and must not share a value"
        );
    }

    /// Reference: pyatv 0.18.0, `pyatv/auth/hap_tlv8.py` — `class TlvValue`
    /// (`SeqNo = 0x06`, `Error = 0x07`, `BackOff = 0x08`, `Flags = 0x13`) and
    /// `class ErrorCode` (`Unknown = 0x01` .. `Busy = 0x07`).
    #[test]
    fn tag_and_error_constants_match_the_hap_specification() {
        assert_eq!(tags::METHOD, 0x00);
        assert_eq!(tags::IDENTIFIER, 0x01);
        assert_eq!(tags::SALT, 0x02);
        assert_eq!(tags::PUBLIC_KEY, 0x03);
        assert_eq!(tags::PROOF, 0x04);
        assert_eq!(tags::ENCRYPTED_DATA, 0x05);
        assert_eq!(tags::STATE, 0x06, "pyatv calls this SeqNo");
        assert_eq!(tags::ERROR, 0x07);
        assert_eq!(tags::RETRY_DELAY, 0x08, "pyatv calls this BackOff");
        assert_eq!(tags::CERTIFICATE, 0x09);
        assert_eq!(tags::SIGNATURE, 0x0A);
        assert_eq!(tags::PERMISSIONS, 0x0B);
        assert_eq!(tags::FRAGMENT_DATA, 0x0C);
        assert_eq!(tags::FRAGMENT_LAST, 0x0D);
        assert_eq!(tags::FLAGS, 0x13);

        assert_eq!(errors::NONE, 0x00);
        assert_eq!(errors::UNKNOWN, 0x01);
        assert_eq!(errors::AUTHENTICATION, 0x02);
        assert_eq!(errors::BACKOFF, 0x03);
        assert_eq!(errors::MAX_PEERS, 0x04);
        assert_eq!(errors::MAX_TRIES, 0x05);
        assert_eq!(errors::UNAVAILABLE, 0x06);
        assert_eq!(errors::BUSY, 0x07);
    }
}
