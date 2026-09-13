//! Proves the WebRTC session layer actually negotiates and carries video.
//!
//! Two `webrtcbin`s in one process, wired to each other through the same
//! [`WebRtcPeer`] API the sender and receiver use, with the signaling channel
//! replaced by a pair of in-process queues. What this pins that a unit test
//! cannot:
//!
//! - `create-offer` produces an SDP that actually contains a media line. An
//!   offer with no `m=` section is what unfixed payloader caps produce, and it
//!   fails silently — the session connects and no video ever arrives.
//! - the offer/answer exchange completes through `WebRtcPeer` in both roles;
//! - trickled ICE reaches the peer and the connection reaches `Connected`;
//! - decoded RGBA frames come out of the far end.
//!
//! The test needs the `nice` GStreamer plugin (Debian/Ubuntu:
//! `gstreamer1.0-nice`). `webrtcbin` builds happily without it and then refuses
//! every pad request, so its absence is reported rather than silently skipped.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::prelude::*;
use openplay_pipeline::{ReceiverPipeline, Role, SdpKind, WebRtcEvent, WebRtcPeer};
use tokio::sync::mpsc;

/// Long enough for a loopback handshake on a loaded CI runner, short enough
/// that a hang fails the run instead of stalling it.
const DEADLINE: Duration = Duration::from_secs(20);

/// Whether this machine can run a WebRTC session at all.
fn nice_plugin_present() -> bool {
    gst::ElementFactory::find("nicesrc").is_some()
        && gst::ElementFactory::find("nicesink").is_some()
}

/// A sender-shaped pipeline fed by `videotestsrc`.
///
/// Deliberately mirrors `SenderPipeline`'s graph — encoder, parser, payloader
/// and the fixed RTP capsfilter — rather than reusing it, because the real one
/// is hardwired to a PipeWire screen-capture source that needs a desktop portal
/// and a user click.
fn build_test_sender() -> gst::Pipeline {
    let pipeline = gst::Pipeline::with_name("test-sender");

    let src = gst::ElementFactory::make("videotestsrc")
        .property("is-live", true)
        .property("num-buffers", 240i32)
        .build()
        .expect("videotestsrc");
    let raw_caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-raw")
                .field("width", 320i32)
                .field("height", 240i32)
                .field("framerate", gst::Fraction::new(30, 1))
                .build(),
        )
        .build()
        .expect("raw capsfilter");
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .expect("videoconvert");
    let encoder = gst::ElementFactory::make("x264enc")
        .property_from_str("tune", "zerolatency")
        .property_from_str("speed-preset", "ultrafast")
        .property("key-int-max", 30u32)
        .property("bframes", 0u32)
        .build()
        .expect("x264enc");
    let h264_caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("video/x-h264")
                .field("profile", "constrained-baseline")
                .field("stream-format", "byte-stream")
                .build(),
        )
        .build()
        .expect("h264 capsfilter");
    let parse = gst::ElementFactory::make("h264parse")
        .property("config-interval", -1i32)
        .build()
        .expect("h264parse");
    let pay = gst::ElementFactory::make("rtph264pay")
        .property("config-interval", -1i32)
        .property_from_str("aggregate-mode", "zero-latency")
        .property("mtu", 1200u32)
        .property("pt", 96u32)
        .build()
        .expect("rtph264pay");
    // The field this test exists to defend.
    let rtp_caps = gst::ElementFactory::make("capsfilter")
        .property(
            "caps",
            gst::Caps::builder("application/x-rtp")
                .field("media", "video")
                .field("encoding-name", "H264")
                .field("payload", 96i32)
                .field("clock-rate", 90000i32)
                .build(),
        )
        .build()
        .expect("rtp capsfilter");
    let webrtcbin = gst::ElementFactory::make("webrtcbin")
        .name("send")
        .property_from_str("bundle-policy", "max-bundle")
        .build()
        .expect("webrtcbin");

    let chain = [
        &src, &raw_caps, &convert, &encoder, &h264_caps, &parse, &pay, &rtp_caps,
    ];
    pipeline.add_many(chain).expect("add sender elements");
    pipeline.add(&webrtcbin).expect("add webrtcbin");
    gst::Element::link_many(chain).expect("link sender chain");

    let sink_pad = webrtcbin
        .request_pad_simple("sink_%u")
        .expect("webrtcbin refused a sink pad — is the nice plugin installed?");
    rtp_caps
        .static_pad("src")
        .expect("rtp_caps src pad")
        .link(&sink_pad)
        .expect("link into webrtcbin");

    pipeline
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_peers_negotiate_and_carry_video() {
    gst::init().expect("GStreamer init");

    assert!(
        nice_plugin_present(),
        "the `nice` GStreamer plugin is missing, so webrtcbin cannot create a \
         single pad and no WebRTC session is possible on this machine. \
         Install `gstreamer1.0-nice` (Debian/Ubuntu) and re-run."
    );

    let frames = Arc::new(AtomicUsize::new(0));
    let first_frame_size = Arc::new(Mutex::new(None::<(u32, u32)>));

    // ── Receiver ──
    let counter = Arc::clone(&frames);
    let size_slot = Arc::clone(&first_frame_size);
    let receiver = ReceiverPipeline::with_frame_handler(Arc::new(move |frame| {
        // The packing contract: tightly packed RGBA, no row padding.
        assert_eq!(
            frame.rgba.len(),
            frame.width as usize * frame.height as usize * 4,
            "frame must be tightly packed RGBA"
        );
        *size_slot.lock().unwrap() = Some((frame.width, frame.height));
        counter.fetch_add(1, Ordering::Relaxed);
    }))
    .expect("build the receiver pipeline");

    let (rx_events_tx, mut rx_events) = mpsc::unbounded_channel::<WebRtcEvent>();
    let receiver_peer = WebRtcPeer::new(receiver.webrtcbin(), Role::Answerer, rx_events_tx);

    // ── Sender ──
    let sender = build_test_sender();
    let sender_webrtc = sender.by_name("send").expect("sender webrtcbin");
    let (tx_events_tx, mut tx_events) = mpsc::unbounded_channel::<WebRtcEvent>();
    let sender_peer = WebRtcPeer::new(&sender_webrtc, Role::Offerer, tx_events_tx);

    // Both must be playing before negotiation: webrtcbin gathers no candidates
    // and produces no offer while it is not running.
    receiver.start().expect("start the receiver pipeline");
    sender
        .set_state(gst::State::Playing)
        .expect("start the sender pipeline");

    let connected = Arc::new(AtomicBool::new(false));
    let saw_offer_with_media = Arc::new(AtomicBool::new(false));

    let deadline = Instant::now() + DEADLINE;
    while Instant::now() < deadline {
        if connected.load(Ordering::Relaxed) && frames.load(Ordering::Relaxed) > 5 {
            break;
        }

        tokio::select! {
            Some(event) = tx_events.recv() => match event {
                WebRtcEvent::LocalDescription { kind, sdp } => {
                    assert_eq!(kind, SdpKind::Offer, "the offerer must produce an offer");
                    // The whole point of the RTP capsfilter. An offer with no
                    // m-line negotiates cleanly and carries nothing.
                    assert!(
                        sdp.lines().any(|l| l.starts_with("m=video")),
                        "offer has no video media line — the payloader's caps are \
                         not fixed:\n{sdp}"
                    );
                    saw_offer_with_media.store(true, Ordering::Relaxed);
                    receiver_peer
                        .set_remote_description(SdpKind::Offer, &sdp)
                        .expect("receiver accepts the offer");
                }
                WebRtcEvent::IceCandidate { sdp_mline_index, candidate } => {
                    receiver_peer.add_ice_candidate(sdp_mline_index, &candidate);
                }
                WebRtcEvent::Failed(reason) => panic!("sender failed: {reason}"),
                _ => {}
            },

            Some(event) = rx_events.recv() => match event {
                WebRtcEvent::LocalDescription { kind, sdp } => {
                    assert_eq!(kind, SdpKind::Answer, "the answerer must produce an answer");
                    sender_peer
                        .set_remote_description(SdpKind::Answer, &sdp)
                        .expect("sender accepts the answer");
                }
                WebRtcEvent::IceCandidate { sdp_mline_index, candidate } => {
                    sender_peer.add_ice_candidate(sdp_mline_index, &candidate);
                }
                WebRtcEvent::Connected => connected.store(true, Ordering::Relaxed),
                WebRtcEvent::Failed(reason) => panic!("receiver failed: {reason}"),
                _ => {}
            },

            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    let _ = sender.set_state(gst::State::Null);
    let _ = receiver.stop();

    assert!(
        saw_offer_with_media.load(Ordering::Relaxed),
        "the sender never produced an offer within {DEADLINE:?}"
    );
    assert!(
        connected.load(Ordering::Relaxed),
        "the peer connection never reached Connected within {DEADLINE:?}"
    );

    let count = frames.load(Ordering::Relaxed);
    assert!(
        count > 5,
        "expected decoded frames to arrive, got {count} within {DEADLINE:?}"
    );

    let size = first_frame_size
        .lock()
        .unwrap()
        .expect("a frame was decoded");
    assert_eq!(
        size,
        (320, 240),
        "decoded frame size should match the source"
    );
}
