/// AirPlay feature flags parsed from the 64-bit bitmask in mDNS TXT records.
///
/// The `features` field in AirPlay TXT records is a 64-bit hex value
/// (sometimes split as two 32-bit hex values separated by a comma).
#[derive(Debug, Clone, Copy, Default)]
pub struct AirPlayFeatures {
    raw: u64,
}

// Known AirPlay feature bits
const FEATURE_VIDEO: u64 = 1 << 0;
const FEATURE_PHOTO: u64 = 1 << 1;
const FEATURE_SCREEN: u64 = 1 << 7; // Screen mirroring
const FEATURE_AUDIO: u64 = 1 << 9;
const FEATURE_SUPPORTS_HK_PAIRING: u64 = 1 << 46;
const FEATURE_SUPPORTS_TRANSIENT_PAIRING: u64 = 1 << 48;

impl AirPlayFeatures {
    /// Parse features from a hex string.
    ///
    /// Handles both formats:
    /// - Single hex: `"0x5A7FFFF7"`
    /// - Split hex: `"0x5A7FFFF7,0x1E"` (lo32,hi32)
    ///
    /// Returns `None` if the input is not valid hex. Note that this is
    /// distinct from `Some(0)`: a receiver may legitimately advertise a
    /// features value of zero, and callers need to tell "advertises no
    /// features" apart from "the field could not be read", since every
    /// `supports_*` predicate reports `false` in both cases.
    pub fn parse(s: &str) -> Option<Self> {
        let raw = if let Some((lo_str, hi_str)) = s.split_once(',') {
            let lo = parse_hex(lo_str.trim())?;
            let hi = parse_hex(hi_str.trim())?;
            (hi << 32) | lo
        } else {
            parse_hex(s.trim())?
        };
        Some(Self { raw })
    }

    /// Raw 64-bit feature value.
    pub fn raw(&self) -> u64 {
        self.raw
    }

    /// Whether the receiver supports screen mirroring (bit 7).
    pub fn supports_mirroring(&self) -> bool {
        self.raw & FEATURE_SCREEN != 0
    }

    /// Whether the receiver supports video playback (bit 0).
    pub fn supports_video(&self) -> bool {
        self.raw & FEATURE_VIDEO != 0
    }

    /// Whether the receiver supports photo display (bit 1).
    pub fn supports_photo(&self) -> bool {
        self.raw & FEATURE_PHOTO != 0
    }

    /// Whether the receiver supports audio streaming (bit 9).
    pub fn supports_audio(&self) -> bool {
        self.raw & FEATURE_AUDIO != 0
    }

    /// Whether the receiver requires HomeKit pairing (bit 46).
    pub fn requires_hk_pairing(&self) -> bool {
        self.raw & FEATURE_SUPPORTS_HK_PAIRING != 0
    }

    /// Whether the receiver supports transient pairing (bit 48).
    pub fn supports_transient_pairing(&self) -> bool {
        self.raw & FEATURE_SUPPORTS_TRANSIENT_PAIRING != 0
    }
}

fn parse_hex(s: &str) -> Option<u64> {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u64::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_hex() {
        let f = AirPlayFeatures::parse("0x5A7FFFF7").unwrap();
        assert!(f.supports_mirroring()); // bit 7 is set
        assert!(f.supports_video()); // bit 0 is set
    }

    #[test]
    fn test_parse_split_hex() {
        // lo=0x5A7FFFF7, hi=0x1E → combined = 0x1E_5A7FFFF7
        let f = AirPlayFeatures::parse("0x5A7FFFF7,0x1E").unwrap();
        assert!(f.supports_mirroring());
        assert_eq!(f.raw() & 0xFFFFFFFF, 0x5A7FFFF7);
    }

    #[test]
    fn test_no_mirroring() {
        // bit 7 = 0x80, so 0x77 has bits 0-6 set but NOT bit 7
        let f = AirPlayFeatures::parse("0x77").unwrap();
        assert!(!f.supports_mirroring());
        assert!(f.supports_video());
    }

    #[test]
    fn test_mirroring_only() {
        let f = AirPlayFeatures::parse("0x80").unwrap();
        assert!(f.supports_mirroring());
        assert!(!f.supports_video());
    }

    #[test]
    fn test_invalid_hex_is_none() {
        assert!(AirPlayFeatures::parse("not_hex").is_none());
        assert!(AirPlayFeatures::parse("").is_none());
        assert!(AirPlayFeatures::parse("0xZZ").is_none());
        // A malformed half of a split value invalidates the whole thing.
        assert!(AirPlayFeatures::parse("0x1E,nope").is_none());
    }

    #[test]
    fn test_zero_is_some_not_none() {
        // A genuine zero must stay distinguishable from a parse failure.
        let f = AirPlayFeatures::parse("0x0").expect("0x0 is valid hex");
        assert_eq!(f.raw(), 0);
        assert!(!f.supports_mirroring());
    }

    #[test]
    fn test_hk_pairing_bit() {
        // bit 46 = 0x400000000000
        let f = AirPlayFeatures::parse("0x0,0x4000").unwrap();
        assert!(f.requires_hk_pairing());
    }
}
