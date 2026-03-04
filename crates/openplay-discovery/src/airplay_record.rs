/// Parsed AirPlay mDNS TXT record fields.
#[derive(Debug, Clone)]
pub struct AirPlayTxtRecord {
    /// Device ID (MAC address).
    pub device_id: String,
    /// Feature bitmask (hex string, e.g. "0x5A7FFFF7,0x1E").
    pub features: String,
    /// Device model (e.g. "AppleTV5,3").
    pub model: String,
    /// Public key (hex).
    pub pk: String,
    /// Flags value.
    pub flags: String,
    /// Source version.
    pub source_version: String,
    /// Protocol version.
    pub protocol_version: String,
}

impl AirPlayTxtRecord {
    /// Parses AirPlay TXT record from mDNS key=value properties.
    pub fn from_properties(props: &[(String, String)]) -> Option<Self> {
        let get = |key: &str| -> Option<String> {
            props
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };

        // AirPlay TXT records must at least have deviceid + features
        let device_id = get("deviceid")?;
        let features = get("features").unwrap_or_default();

        Some(Self {
            device_id,
            features,
            model: get("model").unwrap_or_default(),
            pk: get("pk").unwrap_or_default(),
            flags: get("flags").unwrap_or_default(),
            source_version: get("srcvers").unwrap_or_default(),
            protocol_version: get("protovers").unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_airplay_txt_record() {
        let props = vec![
            ("deviceid".to_string(), "AA:BB:CC:DD:EE:FF".to_string()),
            ("features".to_string(), "0x5A7FFFF7,0x1E".to_string()),
            ("model".to_string(), "AppleTV5,3".to_string()),
            ("pk".to_string(), "abcdef0123456789".to_string()),
            ("flags".to_string(), "0x4".to_string()),
            ("srcvers".to_string(), "220.68".to_string()),
            ("protovers".to_string(), "1.1".to_string()),
        ];

        let record = AirPlayTxtRecord::from_properties(&props).unwrap();
        assert_eq!(record.device_id, "AA:BB:CC:DD:EE:FF");
        assert_eq!(record.features, "0x5A7FFFF7,0x1E");
        assert_eq!(record.model, "AppleTV5,3");
    }

    #[test]
    fn test_parse_minimal_txt_record() {
        let props = vec![
            ("deviceid".to_string(), "11:22:33:44:55:66".to_string()),
        ];

        let record = AirPlayTxtRecord::from_properties(&props).unwrap();
        assert_eq!(record.device_id, "11:22:33:44:55:66");
        assert!(record.features.is_empty());
    }

    #[test]
    fn test_parse_missing_deviceid() {
        let props = vec![
            ("features".to_string(), "0x5A7FFFF7".to_string()),
        ];
        assert!(AirPlayTxtRecord::from_properties(&props).is_none());
    }
}
