use serde::{Deserialize, Serialize};

/// All signaling messages exchanged over the WebSocket connection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    // ── Session negotiation ──
    SessionRequest {
        sender_id: String,
        display_name: String,
        protocol_version: u32,
        capabilities: Capabilities,
    },
    SessionAccept {
        receiver_id: String,
        negotiated: NegotiatedParams,
    },
    SessionReject {
        reason: RejectReason,
    },

    // ── Pairing (first connection) ──
    PairingChallenge {
        receiver_pub_ecdh: String,
    },
    PairingResponse {
        sender_pub_ecdh: String,
        pin_proof: String,
    },
    PairingConfirm {
        confirm: String,
        receiver_cert_fingerprint: String,
    },

    // ── Authentication (subsequent connections) ──
    AuthChallenge {
        nonce: String,
    },
    AuthResponse {
        nonce: String,
        proof: String,
    },
    AuthConfirm {
        proof: String,
    },

    // ── WebRTC signaling ──
    SdpOffer {
        sdp: String,
    },
    SdpAnswer {
        sdp: String,
    },
    IceCandidate {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u32>,
    },
    IceComplete,

    // ── Session control ──
    BitrateHint {
        target_kbps: u32,
        reason: BitrateHintReason,
    },
    Ping {
        timestamp_ms: u64,
    },
    Pong {
        timestamp_ms: u64,
        receiver_timestamp_ms: u64,
    },
    SessionEnd {
        reason: SessionEndReason,
    },
}

/// Capabilities advertised by the sender during session request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capabilities {
    pub video_codecs: Vec<String>,
    pub audio_codecs: Vec<String>,
    pub max_resolution: Option<Resolution>,
    pub max_framerate: Option<u32>,
    pub supports_cursor: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            video_codecs: vec!["h264".to_string()],
            audio_codecs: vec!["opus".to_string()],
            max_resolution: None,
            max_framerate: Some(60),
            supports_cursor: true,
        }
    }
}

/// Screen resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// Parameters negotiated between sender and receiver.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NegotiatedParams {
    pub video_codec: String,
    pub audio_codec: Option<String>,
    pub max_bitrate_kbps: u32,
    pub framerate: u32,
}

/// Reason for rejecting a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    Busy,
    VersionMismatch,
    NoCompatibleCodecs,
    NotPaired,
    Denied,
}

/// Reason for a bitrate hint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BitrateHintReason {
    PacketLoss,
    HighRtt,
    Recovery,
}

/// Reason for ending a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEndReason {
    UserStopped,
    Error,
    Timeout,
    NetworkLost,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_session_request() {
        let msg = SignalingMessage::SessionRequest {
            sender_id: "abc-123".to_string(),
            display_name: "My Laptop".to_string(),
            protocol_version: 1,
            capabilities: Capabilities::default(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"session_request\""));
        assert!(json.contains("\"sender_id\":\"abc-123\""));

        let deserialized: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_serialize_sdp_offer() {
        let msg = SignalingMessage::SdpOffer {
            sdp: "v=0\r\n...".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"sdp_offer\""));

        let deserialized: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_serialize_ice_candidate() {
        let msg = SignalingMessage::IceCandidate {
            candidate: "candidate:1 1 UDP 2122252543 192.168.1.5 50000 typ host".to_string(),
            sdp_mid: Some("0".to_string()),
            sdp_mline_index: Some(0),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_serialize_session_end() {
        let msg = SignalingMessage::SessionEnd {
            reason: SessionEndReason::UserStopped,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"reason\":\"user_stopped\""));
    }

    #[test]
    fn test_serialize_all_variants() {
        let messages = vec![
            SignalingMessage::SessionReject {
                reason: RejectReason::Busy,
            },
            SignalingMessage::PairingChallenge {
                receiver_pub_ecdh: "key".to_string(),
            },
            SignalingMessage::AuthChallenge {
                nonce: "nonce123".to_string(),
            },
            SignalingMessage::IceComplete,
            SignalingMessage::BitrateHint {
                target_kbps: 4000,
                reason: BitrateHintReason::PacketLoss,
            },
            SignalingMessage::Ping {
                timestamp_ms: 1000,
            },
            SignalingMessage::Pong {
                timestamp_ms: 1000,
                receiver_timestamp_ms: 1001,
            },
        ];

        for msg in &messages {
            let json = serde_json::to_string(msg).unwrap();
            let deserialized: SignalingMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, &deserialized);
        }
    }
}
