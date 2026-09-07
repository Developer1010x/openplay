//! End-to-end proof that `SignalingServer` and `SignalingClient` talk to each
//! other, in one process, over a loopback TLS WebSocket.
//!
//! The point is to exercise the whole transport — certificate generation, the
//! rustls handshake against a pinned fingerprint, the WebSocket upgrade, JSON
//! framing, message validation and both channel pairs — without a second
//! machine, so the OpenPlay signaling path is regression-tested in CI.

use openplay_crypto::{client_config_pinned, CertificateManager};
use openplay_protocol::{
    Capabilities, NegotiatedParams, Resolution, SessionEndReason, SignalingMessage, MAX_NAME_CHARS,
};
use openplay_signaling::{
    ConnectionId, IncomingMessage, SignalingClient, SignalingError, SignalingServer,
};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use url::Url;

/// Every await in this file is bounded by this. Loopback needs milliseconds;
/// five seconds only has to cover a slow CI runner, and a hang then fails the
/// test naming the hop it was waiting for instead of blocking the suite.
const STEP: Duration = Duration::from_secs(5);

/// Matches `openplay_common::PROTOCOL_VERSION`. Hardcoded rather than imported
/// so this test does not pull `openplay-common` into the signaling crate.
const PROTOCOL_VERSION: u32 = 1;

type ServerInbox = mpsc::Receiver<IncomingMessage>;
type ServerTask = JoinHandle<anyhow::Result<()>>;

/// Starts a server on an OS-assigned port and returns its real address.
///
/// Binding before spawning is what makes this deterministic: the port is known
/// and the socket is already listening when this returns, so there is no
/// reserve-and-race dance and no readiness polling.
async fn start_server(certs: &CertificateManager) -> (SocketAddr, ServerInbox, ServerTask) {
    let tls_config = certs
        .server_config()
        .expect("build a rustls ServerConfig from the generated certificate");

    let server = SignalingServer::bind(SocketAddr::from(([127, 0, 0, 1], 0)), tls_config)
        .await
        .expect("bind an ephemeral loopback port");
    let addr = server.local_addr().expect("read back the bound port");

    let (inbox_tx, inbox) = mpsc::channel::<IncomingMessage>(32);
    let task = tokio::spawn(async move { server.run(inbox_tx).await });

    (addr, inbox, task)
}

/// Closes the inbox, which is the documented shutdown signal, and asserts that
/// `run` actually returns because of it.
async fn shutdown(inbox: ServerInbox, task: ServerTask) {
    drop(inbox);
    timeout(STEP, task)
        .await
        .expect("run() did not return after its inbox was dropped")
        .expect("the signaling server task panicked")
        .expect("run() should return Ok on a clean shutdown");
}

async fn connect_client(
    addr: SocketAddr,
    fingerprint: &str,
) -> anyhow::Result<(
    mpsc::Sender<SignalingMessage>,
    mpsc::Receiver<SignalingMessage>,
)> {
    let tls_config = client_config_pinned(fingerprint)?;
    let url = Url::parse(&format!("wss://{addr}")).expect("build the receiver URL");
    timeout(STEP, SignalingClient::new(url, tls_config).connect())
        .await
        .expect("timed out during the TLS WebSocket handshake")
}

async fn send_from_sender(tx: &mpsc::Sender<SignalingMessage>, msg: SignalingMessage, what: &str) {
    match timeout(STEP, tx.send(msg)).await {
        Err(_) => panic!("timed out queueing {what} on the sender"),
        Ok(Err(_)) => panic!("the sender's channel closed before {what} could be queued"),
        Ok(Ok(())) => {}
    }
}

async fn recv_at_sender(rx: &mut mpsc::Receiver<SignalingMessage>, what: &str) -> SignalingMessage {
    match timeout(STEP, rx.recv()).await {
        Err(_) => panic!("timed out waiting for {what} at the sender"),
        Ok(None) => panic!("the sender's connection closed before {what} arrived"),
        Ok(Some(msg)) => msg,
    }
}

async fn recv_at_receiver(inbox: &mut ServerInbox, what: &str) -> IncomingMessage {
    match timeout(STEP, inbox.recv()).await {
        Err(_) => panic!("timed out waiting for {what} at the receiver"),
        Ok(None) => panic!("the receiver's inbox closed before {what} arrived"),
        Ok(Some(incoming)) => incoming,
    }
}

/// Checks the per-connection metadata every inbound message carries, and pins
/// the connection identity so a later hop cannot silently come from elsewhere.
fn check_provenance(incoming: &IncomingMessage, expected: Option<ConnectionId>) -> ConnectionId {
    assert!(
        incoming.peer.ip().is_loopback(),
        "expected a loopback peer, got {}",
        incoming.peer
    );
    assert_eq!(
        incoming.connection,
        incoming.reply.id(),
        "the reply handle must belong to the connection the message came from"
    );
    assert!(
        incoming.reply.is_connected(),
        "the reply handle should be live while the sender is still connected"
    );
    if let Some(expected) = expected {
        assert_eq!(
            incoming.connection, expected,
            "every hop of one session must carry the same ConnectionId"
        );
    }
    incoming.connection
}

/// Drives one whole session over the real transport and asserts every hop.
#[tokio::test]
async fn full_session_exchange_over_loopback_tls() {
    let cert_dir = tempfile::tempdir().expect("create a temporary data dir");
    let certs =
        CertificateManager::load_or_generate(cert_dir.path()).expect("generate a certificate");
    let fingerprint = certs.fingerprint().to_string();

    let (addr, mut inbox, server_task) = start_server(&certs).await;

    let (to_receiver, mut from_receiver) = connect_client(addr, &fingerprint)
        .await
        .expect("the pinned TLS WebSocket handshake should succeed");

    // ── Hop 1: sender → receiver, SessionRequest ──
    send_from_sender(
        &to_receiver,
        SignalingMessage::SessionRequest {
            sender_id: "loopback-sender".to_string(),
            display_name: "Loopback Sender".to_string(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Capabilities {
                max_resolution: Some(Resolution {
                    width: 1920,
                    height: 1080,
                }),
                ..Capabilities::default()
            },
        },
        "SessionRequest",
    )
    .await;

    let incoming = recv_at_receiver(&mut inbox, "SessionRequest").await;
    let connection = check_provenance(&incoming, None);
    let reply = incoming.reply;
    match incoming.message {
        SignalingMessage::SessionRequest {
            sender_id,
            display_name,
            protocol_version,
            capabilities,
        } => {
            assert_eq!(sender_id, "loopback-sender");
            assert_eq!(display_name, "Loopback Sender");
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert!(
                capabilities.video_codecs.iter().any(|c| c == "h264"),
                "H.264 should survive the round trip: {:?}",
                capabilities.video_codecs
            );
            assert_eq!(
                capabilities.max_resolution,
                Some(Resolution {
                    width: 1920,
                    height: 1080
                })
            );
        }
        other => panic!("receiver expected SessionRequest, got {other:?}"),
    }

    // ── Hop 2: receiver → sender, SessionAccept ──
    // `ConnectionHandle::send` is synchronous and returns false rather than
    // blocking, so a dropped reply is a test failure, not a hang.
    assert!(
        reply.send(SignalingMessage::SessionAccept {
            receiver_id: "loopback-receiver".to_string(),
            negotiated: NegotiatedParams {
                video_codec: "h264".to_string(),
                audio_codec: None,
                max_bitrate_kbps: 8000,
                framerate: 60,
            },
        }),
        "the receiver could not queue SessionAccept"
    );

    match recv_at_sender(&mut from_receiver, "SessionAccept").await {
        SignalingMessage::SessionAccept {
            receiver_id,
            negotiated,
        } => {
            assert_eq!(receiver_id, "loopback-receiver");
            assert_eq!(negotiated.video_codec, "h264");
            assert_eq!(negotiated.audio_codec, None);
            assert_eq!(negotiated.max_bitrate_kbps, 8000);
            assert_eq!(negotiated.framerate, 60);
        }
        other => panic!("sender expected SessionAccept, got {other:?}"),
    }

    // ── Hop 3: sender → receiver, SdpOffer ──
    // A real offer, not a placeholder: CRLFs and an `=` on every line are
    // exactly the shape a naive framing or escaping bug would mangle.
    let offer_sdp = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
                     m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\n";
    send_from_sender(
        &to_receiver,
        SignalingMessage::SdpOffer {
            sdp: offer_sdp.to_string(),
        },
        "SdpOffer",
    )
    .await;

    let incoming = recv_at_receiver(&mut inbox, "SdpOffer").await;
    check_provenance(&incoming, Some(connection));
    match incoming.message {
        SignalingMessage::SdpOffer { sdp } => assert_eq!(sdp, offer_sdp),
        other => panic!("receiver expected SdpOffer, got {other:?}"),
    }

    // ── Hop 4: receiver → sender, SdpAnswer ──
    // Sent through the handle kept from hop 1, which is how a real receiver
    // answers an offer after the pipeline has produced one.
    let answer_sdp = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
                      m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=rtpmap:96 H264/90000\r\n\
                      a=recvonly\r\n";
    assert!(
        reply.send(SignalingMessage::SdpAnswer {
            sdp: answer_sdp.to_string(),
        }),
        "the receiver could not queue SdpAnswer"
    );

    match recv_at_sender(&mut from_receiver, "SdpAnswer").await {
        SignalingMessage::SdpAnswer { sdp } => assert_eq!(sdp, answer_sdp),
        other => panic!("sender expected SdpAnswer, got {other:?}"),
    }

    // ── Hop 5: sender → receiver, IceCandidate ──
    send_from_sender(
        &to_receiver,
        SignalingMessage::IceCandidate {
            candidate: "candidate:1 1 UDP 2122252543 127.0.0.1 50000 typ host".to_string(),
            sdp_mid: Some("0".to_string()),
            sdp_mline_index: Some(0),
        },
        "IceCandidate (sender → receiver)",
    )
    .await;

    let incoming = recv_at_receiver(&mut inbox, "IceCandidate (sender → receiver)").await;
    check_provenance(&incoming, Some(connection));
    match incoming.message {
        SignalingMessage::IceCandidate {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => {
            assert!(candidate.starts_with("candidate:1 1 UDP"), "{candidate}");
            assert_eq!(sdp_mid.as_deref(), Some("0"));
            assert_eq!(sdp_mline_index, Some(0));
        }
        other => panic!("receiver expected IceCandidate, got {other:?}"),
    }

    // ── Hop 6: receiver → sender, IceCandidate ──
    // The `None` fields matter: serde emits them as JSON null, and a peer that
    // only ever saw `Some` would not catch a regression in that encoding.
    assert!(
        reply.send(SignalingMessage::IceCandidate {
            candidate: "candidate:2 1 UDP 2122252542 127.0.0.1 50001 typ host".to_string(),
            sdp_mid: None,
            sdp_mline_index: None,
        }),
        "the receiver could not queue its own IceCandidate"
    );

    match recv_at_sender(&mut from_receiver, "IceCandidate (receiver → sender)").await {
        SignalingMessage::IceCandidate {
            candidate,
            sdp_mid,
            sdp_mline_index,
        } => {
            assert!(candidate.starts_with("candidate:2 1 UDP"), "{candidate}");
            assert_eq!(sdp_mid, None);
            assert_eq!(sdp_mline_index, None);
        }
        other => panic!("sender expected IceCandidate, got {other:?}"),
    }

    // ── Hop 7: sender → receiver, SessionEnd ──
    send_from_sender(
        &to_receiver,
        SignalingMessage::SessionEnd {
            reason: SessionEndReason::UserStopped,
        },
        "SessionEnd",
    )
    .await;

    let incoming = recv_at_receiver(&mut inbox, "SessionEnd").await;
    check_provenance(&incoming, Some(connection));
    match incoming.message {
        SignalingMessage::SessionEnd { reason } => {
            assert_eq!(reason, SessionEndReason::UserStopped);
        }
        other => panic!("receiver expected SessionEnd, got {other:?}"),
    }

    // Closing the inbox is the documented shutdown signal; prove it works.
    shutdown(inbox, server_task).await;
    drop(cert_dir);
}

/// The pinned fingerprint is really plumbed through `SignalingClient` rather
/// than accepted and ignored: a client pinning someone else's certificate must
/// fail to connect, and the same server must still accept a correct one.
#[tokio::test]
async fn a_client_pinning_the_wrong_certificate_is_refused() {
    let cert_dir = tempfile::tempdir().expect("create a temporary data dir");
    let receiver_certs =
        CertificateManager::load_or_generate(cert_dir.path()).expect("generate a certificate");
    let impostor = CertificateManager::generate().expect("generate a second certificate");
    assert_ne!(receiver_certs.fingerprint(), impostor.fingerprint());

    let (addr, inbox, server_task) = start_server(&receiver_certs).await;

    let err = connect_client(addr, impostor.fingerprint())
        .await
        .expect_err("a mismatched pin must not produce a usable connection");

    match err.downcast_ref::<SignalingError>() {
        Some(SignalingError::Connection(msg)) => assert!(
            msg.contains("fingerprint mismatch"),
            "the pin should be what refused this, not something else: {msg}"
        ),
        _ => panic!("expected SignalingError::Connection, got: {err:?}"),
    }

    // The refusal above was the pin, not a dead listener.
    let (_tx, _rx) = connect_client(addr, receiver_certs.fingerprint())
        .await
        .expect("the correctly pinned client should still connect");

    shutdown(inbox, server_task).await;
    drop(cert_dir);
}

/// `SignalingMessage::validate` is wired into the server's read loop, so a
/// message that violates a documented bound never reaches the application.
///
/// Asserted positively rather than by waiting for nothing to happen: one TCP
/// connection delivers in order, so if the following `Ping` arrives first, the
/// oversized `SessionRequest` was dropped.
#[tokio::test]
async fn the_server_drops_messages_that_fail_validation() {
    let cert_dir = tempfile::tempdir().expect("create a temporary data dir");
    let certs =
        CertificateManager::load_or_generate(cert_dir.path()).expect("generate a certificate");

    let (addr, mut inbox, server_task) = start_server(&certs).await;
    let (to_receiver, _from_receiver) = connect_client(addr, certs.fingerprint())
        .await
        .expect("the pinned TLS WebSocket handshake should succeed");

    send_from_sender(
        &to_receiver,
        SignalingMessage::SessionRequest {
            sender_id: "loopback-sender".to_string(),
            display_name: "n".repeat(MAX_NAME_CHARS + 1),
            protocol_version: PROTOCOL_VERSION,
            capabilities: Capabilities::default(),
        },
        "an over-long SessionRequest",
    )
    .await;

    send_from_sender(
        &to_receiver,
        SignalingMessage::Ping { timestamp_ms: 42 },
        "Ping",
    )
    .await;

    let incoming = recv_at_receiver(&mut inbox, "Ping").await;
    match incoming.message {
        SignalingMessage::Ping { timestamp_ms } => assert_eq!(timestamp_ms, 42),
        other => panic!(
            "the over-long SessionRequest should have been dropped before the \
             application saw it, but the receiver got {other:?}"
        ),
    }

    shutdown(inbox, server_task).await;
    drop(cert_dir);
}
