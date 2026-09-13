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

/// Longest accepted identifier or display name, in characters.
///
/// `display_name` is rendered on the receiver's screen every frame, so an
/// unbounded one is both a spoofing surface and a way to stall the UI thread
/// from the network.
pub const MAX_NAME_CHARS: usize = 64;

/// Longest accepted SDP blob, in bytes.
pub const MAX_SDP_BYTES: usize = 16 * 1024;

/// Longest accepted ICE candidate line, in bytes.
pub const MAX_CANDIDATE_BYTES: usize = 1024;

/// Longest accepted pairing or authentication blob, in bytes.
pub const MAX_PROOF_BYTES: usize = 1024;

/// Most codec names accepted in one capability list.
pub const MAX_CODECS: usize = 16;

/// Longest accepted codec name, in characters.
pub const MAX_CODEC_CHARS: usize = 32;

/// Why a message was rejected before it reached any state machine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(String);

impl SignalingMessage {
    /// Checks that every field is within its documented bound.
    ///
    /// Call this immediately after deserializing a message from the network and
    /// before acting on it. Serde enforces the *shape* of a message but not its
    /// size, and every `String` and `Vec` here is attacker-controlled: without
    /// this the ceiling is the WebSocket frame limit, which is far larger than
    /// anything the protocol legitimately carries.
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            SignalingMessage::SessionRequest {
                sender_id,
                display_name,
                capabilities,
                ..
            } => {
                check_name("sender_id", sender_id)?;
                check_name("display_name", display_name)?;
                check_codecs("video_codecs", &capabilities.video_codecs)?;
                check_codecs("audio_codecs", &capabilities.audio_codecs)?;
            }
            SignalingMessage::SessionAccept { receiver_id, .. } => {
                check_name("receiver_id", receiver_id)?;
            }
            SignalingMessage::SdpOffer { sdp } | SignalingMessage::SdpAnswer { sdp } => {
                check_len("sdp", sdp.len(), MAX_SDP_BYTES)?;
            }
            SignalingMessage::IceCandidate {
                candidate, sdp_mid, ..
            } => {
                check_len("candidate", candidate.len(), MAX_CANDIDATE_BYTES)?;
                if let Some(mid) = sdp_mid {
                    check_len("sdp_mid", mid.len(), MAX_NAME_CHARS)?;
                }
            }
            SignalingMessage::PairingChallenge { receiver_pub_ecdh } => {
                check_len(
                    "receiver_pub_ecdh",
                    receiver_pub_ecdh.len(),
                    MAX_PROOF_BYTES,
                )?;
            }
            SignalingMessage::PairingResponse {
                sender_pub_ecdh,
                pin_proof,
            } => {
                check_len("sender_pub_ecdh", sender_pub_ecdh.len(), MAX_PROOF_BYTES)?;
                check_len("pin_proof", pin_proof.len(), MAX_PROOF_BYTES)?;
            }
            SignalingMessage::PairingConfirm {
                confirm,
                receiver_cert_fingerprint,
            } => {
                check_len("confirm", confirm.len(), MAX_PROOF_BYTES)?;
                check_len(
                    "receiver_cert_fingerprint",
                    receiver_cert_fingerprint.len(),
                    MAX_PROOF_BYTES,
                )?;
            }
            SignalingMessage::AuthChallenge { nonce } => {
                check_len("nonce", nonce.len(), MAX_PROOF_BYTES)?;
            }
            SignalingMessage::AuthResponse { nonce, proof } => {
                check_len("nonce", nonce.len(), MAX_PROOF_BYTES)?;
                check_len("proof", proof.len(), MAX_PROOF_BYTES)?;
            }
            SignalingMessage::AuthConfirm { proof } => {
                check_len("proof", proof.len(), MAX_PROOF_BYTES)?;
            }
            SignalingMessage::IceComplete
            | SignalingMessage::SessionReject { .. }
            | SignalingMessage::BitrateHint { .. }
            | SignalingMessage::Ping { .. }
            | SignalingMessage::Pong { .. }
            | SignalingMessage::SessionEnd { .. } => {}
        }
        Ok(())
    }
}

/// Rejects a name that is empty, over-long, or carries control characters.
///
/// Control characters matter beyond rendering: a newline in a name forges log
/// lines in the receiver's structured log.
fn check_name(field: &str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError(format!("{field} must not be empty")));
    }
    let chars = value.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(ValidationError(format!(
            "{field} must be at most {MAX_NAME_CHARS} characters, got {chars}"
        )));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(ValidationError(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

fn check_len(field: &str, len: usize, max: usize) -> Result<(), ValidationError> {
    if len > max {
        return Err(ValidationError(format!(
            "{field} must be at most {max} bytes, got {len}"
        )));
    }
    Ok(())
}

fn check_codecs(field: &str, codecs: &[String]) -> Result<(), ValidationError> {
    if codecs.len() > MAX_CODECS {
        return Err(ValidationError(format!(
            "{field} must list at most {MAX_CODECS} codecs, got {}",
            codecs.len()
        )));
    }
    for codec in codecs {
        let chars = codec.chars().count();
        if chars > MAX_CODEC_CHARS {
            return Err(ValidationError(format!(
                "{field} entries must be at most {MAX_CODEC_CHARS} characters, got {chars}"
            )));
        }
    }
    Ok(())
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
            SignalingMessage::Ping { timestamp_ms: 1000 },
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
