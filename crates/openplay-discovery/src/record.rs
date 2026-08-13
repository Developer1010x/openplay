/// TXT record fields for mDNS service advertisement.
#[derive(Debug, Clone)]
pub struct TxtRecord {
    /// Protocol version.
    pub version: u32,
    /// Human-readable display name.
    pub display_name: String,
    /// Supported capabilities (comma-separated: "audio,cursor").
    pub capabilities: String,
    /// Supported video codecs (comma-separated).
    pub video_codecs: String,
    /// Supported audio codecs (comma-separated).
    pub audio_codecs: String,
    /// Maximum resolution ("WIDTHxHEIGHT").
    pub resolution: String,
    /// Maximum framerate.
    pub max_fps: u32,
    /// Certificate fingerprint.
    pub fingerprint: String,
    /// Signaling port.
    pub port: u16,
}

impl TxtRecord {
    /// Converts to key=value pairs for mDNS TXT record.
    pub fn to_properties(&self) -> Vec<(String, String)> {
        vec![
            ("v".to_string(), self.version.to_string()),
            ("dn".to_string(), self.display_name.clone()),
            ("cap".to_string(), self.capabilities.clone()),
            ("vc".to_string(), self.video_codecs.clone()),
            ("ac".to_string(), self.audio_codecs.clone()),
            ("res".to_string(), self.resolution.clone()),
            ("fps".to_string(), self.max_fps.to_string()),
            ("fp".to_string(), self.fingerprint.clone()),
            ("port".to_string(), self.port.to_string()),
        ]
    }

    /// Parses TXT record from key=value pairs.
    pub fn from_properties(props: &[(String, String)]) -> Option<Self> {
        let get = |key: &str| -> Option<String> {
            props.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };

        Some(Self {
            version: get("v")?.parse().ok()?,
            display_name: get("dn")?,
            capabilities: get("cap").unwrap_or_default(),
            video_codecs: get("vc").unwrap_or_else(|| "h264".to_string()),
            audio_codecs: get("ac").unwrap_or_else(|| "opus".to_string()),
            resolution: get("res").unwrap_or_else(|| "1920x1080".to_string()),
            max_fps: get("fps").and_then(|f| f.parse().ok()).unwrap_or(30),
            fingerprint: get("fp").unwrap_or_default(),
            port: get("port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(super::DEFAULT_PORT),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_txt_record_roundtrip() {
        let record = TxtRecord {
            version: 1,
            display_name: "Living Room TV".to_string(),
            capabilities: "audio,cursor".to_string(),
            video_codecs: "h264".to_string(),
            audio_codecs: "opus".to_string(),
            resolution: "3840x2160".to_string(),
            max_fps: 60,
            fingerprint: "A3:B2:C1".to_string(),
            port: 7290,
        };

        let props = record.to_properties();
        let parsed = TxtRecord::from_properties(&props).unwrap();

        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.display_name, "Living Room TV");
        assert_eq!(parsed.capabilities, "audio,cursor");
        assert_eq!(parsed.resolution, "3840x2160");
        assert_eq!(parsed.max_fps, 60);
        assert_eq!(parsed.fingerprint, "A3:B2:C1");
        assert_eq!(parsed.port, 7290);
    }
}
