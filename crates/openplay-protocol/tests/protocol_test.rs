use openplay_protocol::{
    receiver_event_from_message, sender_event_from_message, BitrateHintReason, Capabilities,
    NegotiatedParams, ReceiverEvent, ReceiverStateMachine, RejectReason, Resolution, SenderEvent,
    SenderState, SenderStateMachine, SessionEndReason, SignalingMessage,
};

// ── Message serialization ──────────────────────────────────────────────────────

#[test]
fn session_accept_roundtrip() {
    let msg = SignalingMessage::SessionAccept {
        receiver_id: "rx-42".to_string(),
        negotiated: NegotiatedParams {
            video_codec: "h264".to_string(),
            audio_codec: Some("opus".to_string()),
            max_bitrate_kbps: 6000,
            framerate: 30,
        },
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"session_accept\""));
    let back: SignalingMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn session_reject_all_reasons() {
    for reason in [
        RejectReason::Busy,
        RejectReason::VersionMismatch,
        RejectReason::NoCompatibleCodecs,
        RejectReason::NotPaired,
        RejectReason::Denied,
    ] {
        let msg = SignalingMessage::SessionReject { reason };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn pairing_messages_roundtrip() {
    let msgs = vec![
        SignalingMessage::PairingChallenge {
            receiver_pub_ecdh: "base64key==".to_string(),
        },
        SignalingMessage::PairingResponse {
            sender_pub_ecdh: "senderkey==".to_string(),
            pin_proof: "proof123".to_string(),
        },
        SignalingMessage::PairingConfirm {
            confirm: "conf".to_string(),
            receiver_cert_fingerprint: "AA:BB:CC".to_string(),
        },
    ];
    for msg in msgs {
        let json = serde_json::to_string(&msg).unwrap();
        let back: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn auth_messages_roundtrip() {
    let msgs = vec![
        SignalingMessage::AuthChallenge {
            nonce: "nonce-xyz".to_string(),
        },
        SignalingMessage::AuthResponse {
            nonce: "nonce-xyz".to_string(),
            proof: "hmac-proof".to_string(),
        },
        SignalingMessage::AuthConfirm {
            proof: "server-proof".to_string(),
        },
    ];
    for msg in msgs {
        let json = serde_json::to_string(&msg).unwrap();
        let back: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn sdp_answer_roundtrip() {
    let msg = SignalingMessage::SdpAnswer {
        sdp: "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\n".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"sdp_answer\""));
    let back: SignalingMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn ice_complete_roundtrip() {
    let msg = SignalingMessage::IceComplete;
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"type\":\"ice_complete\""));
    let back: SignalingMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn bitrate_hint_all_reasons() {
    for reason in [
        BitrateHintReason::PacketLoss,
        BitrateHintReason::HighRtt,
        BitrateHintReason::Recovery,
    ] {
        let msg = SignalingMessage::BitrateHint {
            target_kbps: 3000,
            reason,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn ping_pong_roundtrip() {
    let ping = SignalingMessage::Ping {
        timestamp_ms: 99_999,
    };
    let pong = SignalingMessage::Pong {
        timestamp_ms: 99_999,
        receiver_timestamp_ms: 100_001,
    };
    for msg in [ping, pong] {
        let json = serde_json::to_string(&msg).unwrap();
        let back: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn session_end_all_reasons() {
    for reason in [
        SessionEndReason::UserStopped,
        SessionEndReason::Error,
        SessionEndReason::Timeout,
        SessionEndReason::NetworkLost,
    ] {
        let msg = SignalingMessage::SessionEnd { reason };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SignalingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn capabilities_default_has_h264() {
    let caps = Capabilities::default();
    assert!(caps.video_codecs.contains(&"h264".to_string()));
    assert!(!caps.audio_codecs.is_empty());
    assert_eq!(caps.max_framerate, Some(60));
    assert!(caps.supports_cursor);
}

#[test]
fn resolution_roundtrip() {
    let res = Resolution {
        width: 3840,
        height: 2160,
    };
    let json = serde_json::to_string(&res).unwrap();
    let back: Resolution = serde_json::from_str(&json).unwrap();
    assert_eq!(back.width, 3840);
    assert_eq!(back.height, 2160);
}

// ── State machine – sender ─────────────────────────────────────────────────────

#[test]
fn sender_cancel_from_discovering() {
    let mut sm = SenderStateMachine::new();
    sm.transition(&SenderEvent::StartDiscovery).unwrap();
    sm.transition(&SenderEvent::Cancel).unwrap();
    assert_eq!(sm.state(), SenderState::Idle);
}

#[test]
fn sender_timeout_from_connecting() {
    let mut sm = SenderStateMachine::new();
    sm.transition(&SenderEvent::StartDiscovery).unwrap();
    sm.transition(&SenderEvent::ReceiverSelected).unwrap();
    sm.transition(&SenderEvent::Timeout).unwrap();
    assert_eq!(sm.state(), SenderState::Idle);
}

#[test]
fn sender_pairing_failed_resets_to_idle() {
    let mut sm = SenderStateMachine::new();
    sm.transition(&SenderEvent::StartDiscovery).unwrap();
    sm.transition(&SenderEvent::ReceiverSelected).unwrap();
    sm.transition(&SenderEvent::NeedsPairing).unwrap();
    sm.transition(&SenderEvent::PairingFailed).unwrap();
    assert_eq!(sm.state(), SenderState::Idle);
}

#[test]
fn sender_auth_failed_resets_to_idle() {
    let mut sm = SenderStateMachine::new();
    sm.transition(&SenderEvent::StartDiscovery).unwrap();
    sm.transition(&SenderEvent::ReceiverSelected).unwrap();
    sm.transition(&SenderEvent::Connected).unwrap();
    sm.transition(&SenderEvent::AuthFailed).unwrap();
    assert_eq!(sm.state(), SenderState::Idle);
}

#[test]
fn sender_connection_lost_from_streaming() {
    let mut sm = SenderStateMachine::new();
    sm.transition(&SenderEvent::StartDiscovery).unwrap();
    sm.transition(&SenderEvent::ReceiverSelected).unwrap();
    sm.transition(&SenderEvent::Connected).unwrap();
    sm.transition(&SenderEvent::Authenticated).unwrap();
    sm.transition(&SenderEvent::StreamStarted).unwrap();
    sm.transition(&SenderEvent::ConnectionLost).unwrap();
    assert_eq!(sm.state(), SenderState::Disconnecting);
}

#[test]
fn sender_invalid_transition_contains_state_names() {
    let mut sm = SenderStateMachine::new();
    let err = sm.transition(&SenderEvent::StreamStarted).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Idle"));
    assert!(msg.contains("StreamStarted"));
}

#[test]
fn sender_default_equals_new() {
    let a = SenderStateMachine::new();
    let b = SenderStateMachine::default();
    assert_eq!(a.state(), b.state());
}

// ── State machine – receiver ───────────────────────────────────────────────────

#[test]
fn receiver_stop_advertising() {
    let mut sm = ReceiverStateMachine::new();
    sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
    sm.transition(&ReceiverEvent::StopAdvertising).unwrap();
    assert_eq!(sm.state(), openplay_protocol::ReceiverState::Idle);
}

#[test]
fn receiver_rejected_connection_returns_to_advertising() {
    let mut sm = ReceiverStateMachine::new();
    sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
    sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
    sm.transition(&ReceiverEvent::Rejected).unwrap();
    assert_eq!(sm.state(), openplay_protocol::ReceiverState::Advertising);
}

#[test]
fn receiver_pairing_failed_returns_to_advertising() {
    let mut sm = ReceiverStateMachine::new();
    sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
    sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
    sm.transition(&ReceiverEvent::NeedsPairing).unwrap();
    sm.transition(&ReceiverEvent::PairingFailed).unwrap();
    assert_eq!(sm.state(), openplay_protocol::ReceiverState::Advertising);
}

#[test]
fn receiver_auth_failed_returns_to_advertising() {
    let mut sm = ReceiverStateMachine::new();
    sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
    sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
    sm.transition(&ReceiverEvent::AlreadyPaired).unwrap();
    sm.transition(&ReceiverEvent::AuthFailed).unwrap();
    assert_eq!(sm.state(), openplay_protocol::ReceiverState::Advertising);
}

#[test]
fn receiver_signaling_failed_returns_to_advertising() {
    let mut sm = ReceiverStateMachine::new();
    sm.transition(&ReceiverEvent::StartAdvertising).unwrap();
    sm.transition(&ReceiverEvent::IncomingConnection).unwrap();
    sm.transition(&ReceiverEvent::AlreadyPaired).unwrap();
    sm.transition(&ReceiverEvent::Authenticated).unwrap();
    sm.transition(&ReceiverEvent::SignalingFailed).unwrap();
    assert_eq!(sm.state(), openplay_protocol::ReceiverState::Advertising);
}

#[test]
fn receiver_invalid_transition_errors() {
    let mut sm = ReceiverStateMachine::new();
    assert!(sm.transition(&ReceiverEvent::Disconnected).is_err());
}

// ── Message-to-event mapping ───────────────────────────────────────────────────

#[test]
fn sender_event_from_session_accept() {
    let msg = SignalingMessage::SessionAccept {
        receiver_id: "r".to_string(),
        negotiated: NegotiatedParams {
            video_codec: "h264".to_string(),
            audio_codec: None,
            max_bitrate_kbps: 4000,
            framerate: 30,
        },
    };
    assert_eq!(
        sender_event_from_message(&msg),
        Some(SenderEvent::Authenticated)
    );
}

#[test]
fn sender_event_from_session_end() {
    let msg = SignalingMessage::SessionEnd {
        reason: SessionEndReason::NetworkLost,
    };
    assert_eq!(
        sender_event_from_message(&msg),
        Some(SenderEvent::ConnectionLost)
    );
}

#[test]
fn sender_event_from_pairing_challenge() {
    let msg = SignalingMessage::PairingChallenge {
        receiver_pub_ecdh: "key".to_string(),
    };
    assert_eq!(
        sender_event_from_message(&msg),
        Some(SenderEvent::NeedsPairing)
    );
}

#[test]
fn sender_event_none_for_sdp_offer() {
    let msg = SignalingMessage::SdpOffer {
        sdp: "v=0".to_string(),
    };
    assert_eq!(sender_event_from_message(&msg), None);
}

#[test]
fn receiver_event_from_session_request() {
    let msg = SignalingMessage::SessionRequest {
        sender_id: "s".to_string(),
        display_name: "Laptop".to_string(),
        protocol_version: 1,
        capabilities: Capabilities::default(),
    };
    assert_eq!(
        receiver_event_from_message(&msg),
        Some(ReceiverEvent::IncomingConnection)
    );
}

#[test]
fn receiver_event_from_session_end() {
    let msg = SignalingMessage::SessionEnd {
        reason: SessionEndReason::Error,
    };
    assert_eq!(
        receiver_event_from_message(&msg),
        Some(ReceiverEvent::ConnectionLost)
    );
}

#[test]
fn receiver_event_none_for_pairing_response() {
    let msg = SignalingMessage::PairingResponse {
        sender_pub_ecdh: "k".to_string(),
        pin_proof: "p".to_string(),
    };
    assert_eq!(receiver_event_from_message(&msg), None);
}
