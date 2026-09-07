//! The WebRTC session layer: SDP negotiation and trickle ICE.
//!
//! [`SenderPipeline`](crate::SenderPipeline) and
//! [`ReceiverPipeline`](crate::ReceiverPipeline) build the element graphs;
//! neither of them talks to a peer. This module is the part in between — it
//! drives `webrtcbin`'s offer/answer dance and turns its GObject signals into
//! [`WebRtcEvent`]s on a channel the signaling code can await.
//!
//! # Why a channel
//!
//! `webrtcbin` delivers everything on GStreamer streaming threads: promise
//! replies, ICE candidates, state changes. Signaling lives in tokio and the UI
//! lives in egui, so nothing may block and nothing may assume a runtime is
//! entered. An **unbounded** channel is the one primitive that satisfies both:
//! `UnboundedSender::send` is synchronous, never blocks, and never panics for
//! want of a reactor — unlike `Sender::blocking_send`, which panics outright
//! when called from inside a runtime thread. The traffic is a handful of
//! messages per session, so unboundedness costs nothing.
//!
//! # Roles
//!
//! Exactly one peer must offer. [`Role::Offerer`] answers `webrtcbin`'s
//! `on-negotiation-needed` by building an offer; [`Role::Answerer`] ignores
//! that signal and only produces an answer once
//! [`WebRtcPeer::set_remote_description`] has fed it an offer. Both peers
//! offering, or neither, is the classic way to get a session that negotiates
//! forever and carries no video.
//!
//! # Connectivity
//!
//! No STUN or TURN server is configured, so only host candidates are gathered.
//! That is deliberate: OpenPlay casts over the LAN and contacting a third-party
//! STUN server would leak the fact that a cast is happening. Call
//! [`WebRtcPeer::set_stun_server`] before negotiating if a deployment needs it.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::PipelineError;

type Result<T> = std::result::Result<T, PipelineError>;

/// Which half of the offer/answer exchange this peer performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Creates the offer as soon as `webrtcbin` asks for negotiation.
    Offerer,
    /// Waits for a remote offer, then answers it.
    Answerer,
}

/// Whether a session description is an offer or an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdpKind {
    Offer,
    Answer,
}

impl SdpKind {
    fn to_gst(self) -> gst_webrtc::WebRTCSDPType {
        match self {
            SdpKind::Offer => gst_webrtc::WebRTCSDPType::Offer,
            SdpKind::Answer => gst_webrtc::WebRTCSDPType::Answer,
        }
    }
}

/// Everything the session layer reports upward.
///
/// The three that must be forwarded over signaling are [`Self::LocalDescription`],
/// [`Self::IceCandidate`] and [`Self::IceGatheringComplete`]; the rest are for
/// status reporting and teardown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebRtcEvent {
    /// A local offer or answer is ready. Send its `sdp` to the peer.
    LocalDescription { kind: SdpKind, sdp: String },
    /// A local ICE candidate was gathered. Trickle it to the peer.
    IceCandidate {
        sdp_mline_index: u32,
        candidate: String,
    },
    /// ICE gathering finished; no further candidates will be produced.
    IceGatheringComplete,
    /// The peer connection reached `connected`. Media should now flow.
    Connected,
    /// The peer connection failed, was closed, or a negotiation step errored.
    Failed(String),
    /// The peer connection went from connected to disconnected.
    Disconnected,
}

/// Drives SDP negotiation and ICE for one `webrtcbin`.
///
/// Holds a clone of the element, so it is cheap to construct and independent of
/// the pipeline wrapper's lifetime.
pub struct WebRtcPeer {
    webrtcbin: gst::Element,
    role: Role,
}

impl WebRtcPeer {
    /// Attaches to a `webrtcbin` and starts reporting on `events`.
    ///
    /// Connects `on-negotiation-needed`, `on-ice-candidate`, and the two state
    /// notifications. For [`Role::Offerer`] this is enough to produce an offer
    /// unprompted once the pipeline is playing.
    pub fn new(
        webrtcbin: &gst::Element,
        role: Role,
        events: mpsc::UnboundedSender<WebRtcEvent>,
    ) -> Self {
        let peer = Self {
            webrtcbin: webrtcbin.clone(),
            role,
        };

        peer.connect_negotiation_needed(events.clone());
        peer.connect_ice_candidate(events.clone());
        peer.connect_state_notifications(events);

        peer
    }

    /// Points ICE at a STUN server. Must be called before negotiation starts.
    ///
    /// Not used by default — see the module docs on why LAN casting gathers
    /// host candidates only.
    pub fn set_stun_server(&self, uri: &str) {
        self.webrtcbin.set_property("stun-server", uri);
        info!(uri, "STUN server configured");
    }

    /// Applies a remote description.
    ///
    /// When `kind` is [`SdpKind::Offer`] and this peer is an
    /// [`Role::Answerer`], an answer is created automatically and arrives as
    /// [`WebRtcEvent::LocalDescription`].
    pub fn set_remote_description(&self, kind: SdpKind, sdp: &str) -> Result<()> {
        let parsed = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
            .map_err(|e| PipelineError::Sdp(format!("Failed to parse remote {kind:?}: {e}")))?;

        let desc = gst_webrtc::WebRTCSessionDescription::new(kind.to_gst(), parsed);

        self.webrtcbin
            .emit_by_name::<()>("set-remote-description", &[&desc, &None::<gst::Promise>]);

        debug!(?kind, "Remote description set");

        // Only the answerer owes a reply, and only to an offer. Answering our
        // own answer would restart negotiation.
        if kind == SdpKind::Offer && self.role == Role::Answerer {
            self.create_answer();
        }

        Ok(())
    }

    /// Feeds a remote ICE candidate to `webrtcbin`.
    ///
    /// An empty candidate string is the end-of-candidates marker some peers
    /// send; `webrtcbin` treats it as such, so it is passed through unchanged.
    pub fn add_ice_candidate(&self, sdp_mline_index: u32, candidate: &str) {
        self.webrtcbin
            .emit_by_name::<()>("add-ice-candidate", &[&sdp_mline_index, &candidate]);
        debug!(sdp_mline_index, candidate, "Remote ICE candidate added");
    }

    /// Asks `webrtcbin` for an offer. Normally driven by `on-negotiation-needed`.
    pub fn create_offer(&self) {
        Self::request_description(&self.webrtcbin, SdpKind::Offer, self.events_from_signal());
    }

    /// Asks `webrtcbin` for an answer to the offer already set.
    pub fn create_answer(&self) {
        Self::request_description(&self.webrtcbin, SdpKind::Answer, self.events_from_signal());
    }

    /// The event sender stashed on the element by [`Self::new`].
    ///
    /// Keeping it on the element rather than in `self` lets the signal closures
    /// and the inherent methods share one sender without `Arc`-wrapping the
    /// whole peer.
    fn events_from_signal(&self) -> Option<mpsc::UnboundedSender<WebRtcEvent>> {
        unsafe {
            self.webrtcbin
                .data::<mpsc::UnboundedSender<WebRtcEvent>>(EVENTS_KEY)
                .map(|ptr| ptr.as_ref().clone())
        }
    }

    fn connect_negotiation_needed(&self, events: mpsc::UnboundedSender<WebRtcEvent>) {
        // Stash the sender on the element so create_offer/create_answer can
        // reach it later without threading it through every call site.
        unsafe {
            self.webrtcbin.set_data(EVENTS_KEY, events.clone());
        }

        if self.role != Role::Offerer {
            debug!("Answerer: ignoring on-negotiation-needed");
            return;
        }

        self.webrtcbin
            .connect("on-negotiation-needed", false, move |values| {
                let Ok(bin) = values[0].get::<gst::Element>() else {
                    error!("on-negotiation-needed gave a non-element");
                    return None;
                };
                info!("Negotiation needed — creating offer");
                Self::request_description(&bin, SdpKind::Offer, Some(events.clone()));
                None
            });
    }

    fn connect_ice_candidate(&self, events: mpsc::UnboundedSender<WebRtcEvent>) {
        self.webrtcbin
            .connect("on-ice-candidate", false, move |values| {
                // values[0] is the element; 1 is the mline index, 2 the candidate.
                let sdp_mline_index = match values[1].get::<u32>() {
                    Ok(i) => i,
                    Err(e) => {
                        error!("ICE candidate had no mline index: {e}");
                        return None;
                    }
                };
                let candidate = match values[2].get::<String>() {
                    Ok(c) => c,
                    Err(e) => {
                        error!("ICE candidate had no candidate string: {e}");
                        return None;
                    }
                };

                debug!(sdp_mline_index, %candidate, "Local ICE candidate");
                let _ = events.send(WebRtcEvent::IceCandidate {
                    sdp_mline_index,
                    candidate,
                });
                None
            });
    }

    fn connect_state_notifications(&self, events: mpsc::UnboundedSender<WebRtcEvent>) {
        let gathering_events = events.clone();
        self.webrtcbin
            .connect_notify(Some("ice-gathering-state"), move |bin, _| {
                let state =
                    bin.property::<gst_webrtc::WebRTCICEGatheringState>("ice-gathering-state");
                debug!(?state, "ICE gathering state");
                if state == gst_webrtc::WebRTCICEGatheringState::Complete {
                    let _ = gathering_events.send(WebRtcEvent::IceGatheringComplete);
                }
            });

        self.webrtcbin
            .connect_notify(Some("connection-state"), move |bin, _| {
                let state =
                    bin.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
                info!(?state, "WebRTC connection state");
                let event = match state {
                    gst_webrtc::WebRTCPeerConnectionState::Connected => {
                        Some(WebRtcEvent::Connected)
                    }
                    gst_webrtc::WebRTCPeerConnectionState::Disconnected => {
                        Some(WebRtcEvent::Disconnected)
                    }
                    gst_webrtc::WebRTCPeerConnectionState::Failed => {
                        Some(WebRtcEvent::Failed("Peer connection failed".to_string()))
                    }
                    gst_webrtc::WebRTCPeerConnectionState::Closed => {
                        Some(WebRtcEvent::Failed("Peer connection closed".to_string()))
                    }
                    _ => None,
                };
                if let Some(event) = event {
                    let _ = events.send(event);
                }
            });
    }

    /// The shared body of create-offer and create-answer.
    ///
    /// Both signals reply through a [`gst::Promise`] on a GStreamer thread. The
    /// reply is set as the local description before being reported, because the
    /// peer must not receive an SDP we have not committed to ourselves.
    fn request_description(
        webrtcbin: &gst::Element,
        kind: SdpKind,
        events: Option<mpsc::UnboundedSender<WebRtcEvent>>,
    ) {
        let Some(events) = events else {
            error!(
                ?kind,
                "No event channel attached — cannot report the description"
            );
            return;
        };

        let bin = webrtcbin.clone();
        let field = match kind {
            SdpKind::Offer => "offer",
            SdpKind::Answer => "answer",
        };

        let promise = gst::Promise::with_change_func(move |reply| {
            let reply = match reply {
                Ok(Some(reply)) => reply,
                Ok(None) => {
                    let _ = events.send(WebRtcEvent::Failed(format!(
                        "create-{field} returned an empty reply"
                    )));
                    return;
                }
                Err(e) => {
                    let _ =
                        events.send(WebRtcEvent::Failed(format!("create-{field} failed: {e:?}")));
                    return;
                }
            };

            let desc = match reply.get::<gst_webrtc::WebRTCSessionDescription>(field) {
                Ok(d) => d,
                Err(e) => {
                    let _ = events.send(WebRtcEvent::Failed(format!(
                        "create-{field} reply had no {field}: {e}"
                    )));
                    return;
                }
            };

            // Commit locally first, then publish.
            bin.emit_by_name::<()>("set-local-description", &[&desc, &None::<gst::Promise>]);

            let sdp = match desc.sdp().as_text() {
                Ok(text) => text,
                Err(e) => {
                    let _ = events.send(WebRtcEvent::Failed(format!(
                        "Could not serialize the local {field}: {e}"
                    )));
                    return;
                }
            };

            info!(?kind, bytes = sdp.len(), "Local description ready");
            let _ = events.send(WebRtcEvent::LocalDescription { kind, sdp });
        });

        webrtcbin.emit_by_name::<()>(
            &format!("create-{field}"),
            &[&None::<gst::Structure>, &promise],
        );
    }
}

/// Key for the event sender stashed on the `webrtcbin` element.
const EVENTS_KEY: &str = "openplay-webrtc-events";

/// Logs anything unexpected that reaches the pipeline bus.
///
/// A silent WebRTC session is almost always a GStreamer error nobody watched
/// for, so both binaries should install this.
pub fn log_bus_message(_bus: &gst::Bus, message: &gst::Message) -> gst::BusSyncReply {
    use gst::MessageView;
    match message.view() {
        MessageView::Error(err) => {
            error!(
                source = %err.src().map(|s| s.path_string()).unwrap_or_else(|| "unknown".into()),
                error = %err.error(),
                debug = ?err.debug(),
                "GStreamer error"
            );
        }
        MessageView::Warning(w) => {
            warn!(warning = %w.error(), "GStreamer warning");
        }
        _ => {}
    }
    gst::BusSyncReply::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdp_kind_maps_to_gst_types() {
        assert_eq!(SdpKind::Offer.to_gst(), gst_webrtc::WebRTCSDPType::Offer);
        assert_eq!(SdpKind::Answer.to_gst(), gst_webrtc::WebRTCSDPType::Answer);
    }

    #[test]
    fn roles_are_distinct() {
        assert_ne!(Role::Offerer, Role::Answerer);
    }
}
