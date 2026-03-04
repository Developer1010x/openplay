use std::fmt;

/// WFD video codec profiles (CEA resolutions).
///
/// These are bitmasks used in `wfd_video_formats` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CeaResolutions(pub u32);

impl CeaResolutions {
    pub const RES_640X480_P60: u32 = 1 << 0;
    pub const RES_720X480_P60: u32 = 1 << 1;
    pub const RES_720X576_P50: u32 = 1 << 3;
    pub const RES_1280X720_P30: u32 = 1 << 5;
    pub const RES_1280X720_P60: u32 = 1 << 6;
    pub const RES_1920X1080_P30: u32 = 1 << 7;
    pub const RES_1920X1080_P60: u32 = 1 << 8;

    pub fn supports(&self, mask: u32) -> bool {
        self.0 & mask != 0
    }
}

/// WFD audio codec configuration.
#[derive(Debug, Clone)]
pub struct WfdAudioCodecs {
    /// LPCM support bitmask.
    pub lpcm: u32,
    /// AAC support bitmask.
    pub aac: u32,
    /// AC3 support bitmask.
    pub ac3: u32,
}

impl Default for WfdAudioCodecs {
    fn default() -> Self {
        Self {
            lpcm: 0x01, // 44.1kHz, 16-bit, 2ch
            aac: 0x01,  // basic AAC-LC
            ac3: 0,
        }
    }
}

impl fmt::Display for WfdAudioCodecs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LPCM {:08X} 00000002, AAC {:08X} 00000002, AC3 {:08X} 00000002",
            self.lpcm, self.aac, self.ac3
        )
    }
}

/// Negotiated WFD video parameters.
#[derive(Debug, Clone)]
pub struct WfdVideoFormats {
    /// Native resolution index.
    pub native: u8,
    /// Preferred display mode (0 = CEA).
    pub preferred_display_mode: u8,
    /// H.264 profile (1 = CBP, 2 = CHP).
    pub profile: u8,
    /// H.264 level (e.g., 0x08 = level 3.1, 0x10 = level 3.2, 0x20 = level 4).
    pub level: u8,
    /// CEA resolution bitmask.
    pub cea_resolutions: CeaResolutions,
    /// VESA resolution bitmask.
    pub vesa_resolutions: u32,
    /// HH resolution bitmask.
    pub hh_resolutions: u32,
    /// Latency.
    pub latency: u8,
    /// Minimum slice size.
    pub min_slice_size: u16,
    /// Slice encoding parameters.
    pub slice_enc_params: u16,
    /// Frame rate control support.
    pub frame_rate_ctrl: u8,
}

impl Default for WfdVideoFormats {
    fn default() -> Self {
        Self {
            native: 0x00,
            preferred_display_mode: 0,
            profile: 0x01, // CBP
            level: 0x10,   // Level 3.2
            cea_resolutions: CeaResolutions(
                CeaResolutions::RES_1920X1080_P30 | CeaResolutions::RES_1280X720_P30,
            ),
            vesa_resolutions: 0,
            hh_resolutions: 0,
            latency: 0,
            min_slice_size: 0,
            slice_enc_params: 0,
            frame_rate_ctrl: 0x11,
        }
    }
}

impl WfdVideoFormats {
    /// Encodes as WFD parameter string for SET_PARAMETER.
    pub fn encode(&self) -> String {
        format!(
            "{:02X} {:02X} {:02X} {:02X} {:08X} {:08X} {:08X} {:02X} {:04X} {:04X} {:02X}",
            self.native,
            self.preferred_display_mode,
            self.profile,
            self.level,
            self.cea_resolutions.0,
            self.vesa_resolutions,
            self.hh_resolutions,
            self.latency,
            self.min_slice_size,
            self.slice_enc_params,
            self.frame_rate_ctrl,
        )
    }

    /// Parses WFD video formats from a hex parameter string.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.len() < 11 {
            return None;
        }

        Some(Self {
            native: u8::from_str_radix(parts[0], 16).ok()?,
            preferred_display_mode: u8::from_str_radix(parts[1], 16).ok()?,
            profile: u8::from_str_radix(parts[2], 16).ok()?,
            level: u8::from_str_radix(parts[3], 16).ok()?,
            cea_resolutions: CeaResolutions(u32::from_str_radix(parts[4], 16).ok()?),
            vesa_resolutions: u32::from_str_radix(parts[5], 16).ok()?,
            hh_resolutions: u32::from_str_radix(parts[6], 16).ok()?,
            latency: u8::from_str_radix(parts[7], 16).ok()?,
            min_slice_size: u16::from_str_radix(parts[8], 16).ok()?,
            slice_enc_params: u16::from_str_radix(parts[9], 16).ok()?,
            frame_rate_ctrl: u8::from_str_radix(parts[10], 16).ok()?,
        })
    }
}

/// WFD client RTP port configuration.
#[derive(Debug, Clone)]
pub struct WfdClientRtpPorts {
    /// Transport profile.
    pub profile: String,
    /// Primary RTP port.
    pub rtp_port0: u16,
    /// Secondary RTP port (0 = unused).
    pub rtp_port1: u16,
    /// Mode.
    pub mode: String,
}

impl Default for WfdClientRtpPorts {
    fn default() -> Self {
        Self {
            profile: "RTP/AVP/UDP;unicast".to_string(),
            rtp_port0: 1028,
            rtp_port1: 0,
            mode: "mode=play".to_string(),
        }
    }
}

impl fmt::Display for WfdClientRtpPorts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} {}",
            self.profile, self.rtp_port0, self.rtp_port1, self.mode
        )
    }
}

impl WfdClientRtpPorts {
    /// Parse from WFD parameter string.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.len() < 4 {
            return None;
        }
        Some(Self {
            profile: parts[0].to_string(),
            rtp_port0: parts[1].parse().ok()?,
            rtp_port1: parts[2].parse().ok()?,
            mode: parts[3].to_string(),
        })
    }
}

/// Negotiates the best resolution between source and sink capabilities.
pub fn negotiate_resolution(
    source: &WfdVideoFormats,
    sink: &WfdVideoFormats,
) -> (u32, u32, u32) {
    let common_cea = source.cea_resolutions.0 & sink.cea_resolutions.0;

    // Prefer highest resolution
    if common_cea & CeaResolutions::RES_1920X1080_P60 != 0 {
        return (1920, 1080, 60);
    }
    if common_cea & CeaResolutions::RES_1920X1080_P30 != 0 {
        return (1920, 1080, 30);
    }
    if common_cea & CeaResolutions::RES_1280X720_P60 != 0 {
        return (1280, 720, 60);
    }
    if common_cea & CeaResolutions::RES_1280X720_P30 != 0 {
        return (1280, 720, 30);
    }
    if common_cea & CeaResolutions::RES_720X480_P60 != 0 {
        return (720, 480, 60);
    }

    // Fallback
    (1280, 720, 30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_formats_roundtrip() {
        let vf = WfdVideoFormats::default();
        let encoded = vf.encode();
        let parsed = WfdVideoFormats::parse(&encoded).unwrap();
        assert_eq!(parsed.profile, vf.profile);
        assert_eq!(parsed.level, vf.level);
        assert_eq!(parsed.cea_resolutions.0, vf.cea_resolutions.0);
    }

    #[test]
    fn test_cea_resolution_bitmask() {
        let cea = CeaResolutions(CeaResolutions::RES_1920X1080_P30 | CeaResolutions::RES_1280X720_P30);
        assert!(cea.supports(CeaResolutions::RES_1920X1080_P30));
        assert!(cea.supports(CeaResolutions::RES_1280X720_P30));
        assert!(!cea.supports(CeaResolutions::RES_1920X1080_P60));
    }

    #[test]
    fn test_negotiate_resolution_1080p() {
        let source = WfdVideoFormats {
            cea_resolutions: CeaResolutions(
                CeaResolutions::RES_1920X1080_P30 | CeaResolutions::RES_1280X720_P30,
            ),
            ..Default::default()
        };
        let sink = WfdVideoFormats {
            cea_resolutions: CeaResolutions(
                CeaResolutions::RES_1920X1080_P30 | CeaResolutions::RES_1280X720_P60,
            ),
            ..Default::default()
        };
        let (w, h, fps) = negotiate_resolution(&source, &sink);
        assert_eq!((w, h, fps), (1920, 1080, 30));
    }

    #[test]
    fn test_negotiate_resolution_fallback() {
        let source = WfdVideoFormats {
            cea_resolutions: CeaResolutions(CeaResolutions::RES_640X480_P60),
            ..Default::default()
        };
        let sink = WfdVideoFormats {
            cea_resolutions: CeaResolutions(CeaResolutions::RES_1920X1080_P30),
            ..Default::default()
        };
        // No common resolution → fallback
        let (w, h, fps) = negotiate_resolution(&source, &sink);
        assert_eq!((w, h, fps), (1280, 720, 30));
    }

    #[test]
    fn test_client_rtp_ports_display() {
        let ports = WfdClientRtpPorts::default();
        let s = ports.to_string();
        assert!(s.contains("RTP/AVP/UDP;unicast"));
        assert!(s.contains("1028"));
    }

    #[test]
    fn test_client_rtp_ports_parse() {
        let ports = WfdClientRtpPorts::parse("RTP/AVP/UDP;unicast 1028 0 mode=play").unwrap();
        assert_eq!(ports.rtp_port0, 1028);
        assert_eq!(ports.rtp_port1, 0);
    }

    #[test]
    fn test_audio_codecs_display() {
        let ac = WfdAudioCodecs::default();
        let s = ac.to_string();
        assert!(s.contains("LPCM"));
        assert!(s.contains("AAC"));
    }
}
