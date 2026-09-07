//! Discovery, signaling and the receive-side media session.
//!
//! Starts three things at launch and keeps them running for the life of the
//! process:
//!
//! - a [`ReceiverAdvertiser`], which publishes this receiver over mDNS so
//!   senders browsing `_openplay._tcp.local.` can list it;
//! - a [`SignalingServer`], which accepts TLS WebSocket connections on the
//!   configured port and answers session negotiation;
//! - a message loop that, once the user has approved a sender, builds a
//!   [`ReceiverPipeline`] and answers the sender's SDP offer.
//!
//! # Consent
//!
//! Nothing on this path authenticates a sender. mDNS is unauthenticated, the
//! certificate pin it advertises is only as trustworthy as the multicast layer,
//! and the pairing and authentication messages the protocol defines are not
//! implemented. The receiver therefore refuses to display anything until a
//! human sitting in front of it approves the specific sender that asked —
//! [`Status::PendingConsent`] is answered by [`NetHandle::decide`], not by this
//! module. Without that gate, any device on the network could put pixels on
//! the screen unprompted.
//!
//! One session runs at a time, because there is one screen. A second sender is
//! refused with [`RejectReason::Busy`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use openplay_common::{AppConfig, PROTOCOL_VERSION};
use openplay_crypto::CertificateManager;
use openplay_discovery::{ReceiverAdvertiser, TxtRecord};
use openplay_pipeline::{
    log_bus_message, ReceiverPipeline, Role, SdpKind, VideoFrame, WebRtcEvent, WebRtcPeer,
};
use openplay_protocol::{
    Capabilities, NegotiatedParams, RejectReason, SessionEndReason, SignalingMessage,
};
use openplay_signaling::{ConnectionHandle, ConnectionId, IncomingMessage, SignalingServer};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// What the window shows about the current connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Advertised over mDNS, no sender connected.
    Waiting,
    /// A sender asked to cast and is waiting for the user to approve it.
    PendingConsent {
        sender_name: String,
        connection: ConnectionId,
    },
    /// The user approved a sender; negotiating the media session.
    Negotiating { sender_name: String },
    /// Video is arriving.
    Streaming { sender_name: String },
    /// Startup failed; the receiver is not discoverable.
    Failed { reason: String },
}

/// Shared status, written by the async tasks and read by the egui window.
pub type SharedStatus = Arc<Mutex<Status>>;

/// The most recent decoded frame, plus a counter so the window can tell whether
/// it has already uploaded this one.
#[derive(Default)]
pub struct FrameSlot {
    pub frame: Option<VideoFrame>,
    pub sequence: u64,
}

/// Shared frame slot, written from a GStreamer thread and read by the window.
pub type SharedFrame = Arc<Mutex<FrameSlot>>;

/// The user's answer to a [`Status::PendingConsent`] prompt.
#[derive(Debug, Clone, Copy)]
struct Decision {
    connection: ConnectionId,
    accept: bool,
}

/// Owns everything started for the network side.
///
/// Dropping this unregisters the mDNS service (via `ReceiverAdvertiser`'s
/// `Drop`) and shuts the runtime down, so the app must hold it for as long as
/// the window is open.
pub struct NetHandle {
    _runtime: tokio::runtime::Runtime,
    _advertiser: Option<ReceiverAdvertiser>,
    status: SharedStatus,
    frame: SharedFrame,
    decisions: mpsc::UnboundedSender<Decision>,
}

impl NetHandle {
    /// The status cell the window reads each frame.
    pub fn status(&self) -> SharedStatus {
        Arc::clone(&self.status)
    }

    /// The frame slot the window uploads from.
    pub fn frame(&self) -> SharedFrame {
        Arc::clone(&self.frame)
    }

    /// Answers a pending consent prompt.
    ///
    /// The connection id is carried in the [`Status::PendingConsent`] the window
    /// is displaying, so a decision cannot be applied to a different sender that
    /// connected in between.
    pub fn decide(&self, connection: ConnectionId, accept: bool) {
        if self
            .decisions
            .send(Decision { connection, accept })
            .is_err()
        {
            warn!("Consent channel closed — the signaling loop is gone");
        }
    }
}

/// Starts mDNS advertisement, the signaling server and the session loop.
///
/// A failure to advertise is not fatal — the signaling server can still accept
/// a sender that was given the address by hand — so each half is reported
/// separately rather than aborting the app.
pub fn start(config: &AppConfig) -> Result<NetHandle> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to build the tokio runtime")?;

    let data_dir = openplay_common::data_dir();
    let certs = CertificateManager::load_or_generate(&data_dir)
        .context("Failed to load or generate the TLS certificate")?;

    let status: SharedStatus = Arc::new(Mutex::new(Status::Waiting));
    let frame: SharedFrame = Arc::new(Mutex::new(FrameSlot::default()));

    // Advertise first: the fingerprint in the TXT record has to match the
    // certificate the signaling server is about to present.
    //
    // mDNS registration must happen inside the runtime — the daemon spawns
    // work that expects a reactor — so this runs under `enter`.
    let advertiser = {
        let _guard = runtime.enter();
        match ReceiverAdvertiser::new(&txt_record(config, certs.fingerprint())) {
            Ok(adv) => Some(adv),
            Err(e) => {
                error!("mDNS advertisement failed, this receiver will not be listed: {e}");
                None
            }
        }
    };

    // A receiver that cannot advertise is degraded, not broken: a sender given
    // the address by hand still works. Say so instead of showing a hard error
    // over a server that is about to start listening.
    if advertiser.is_none() {
        set_status(
            &status,
            Status::Failed {
                reason:
                    "Not discoverable on this network — senders must be pointed here by address"
                        .to_string(),
            },
        );
    }

    let tls_config = certs
        .server_config()
        .context("Failed to build the TLS server config")?;

    // Bind before reporting success, so a port clash is an error the user sees
    // rather than a log line that arrives after the window says "listening".
    //
    // `[::]` accepts IPv4 too on any dual-stack host, which matters because
    // mDNS advertises every interface address including IPv6 ones; binding
    // IPv4-only would publish addresses nothing listens on. Hosts with IPv6
    // disabled fall back to `0.0.0.0`.
    let server = runtime
        .block_on(async {
            let v6 = SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, config.port));
            match SignalingServer::bind(v6, Arc::clone(&tls_config)).await {
                Ok(server) => Ok(server),
                Err(e) => {
                    debug!("Dual-stack bind failed ({e:#}), falling back to IPv4");
                    let v4 = SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, config.port));
                    SignalingServer::bind(v4, tls_config).await
                }
            }
        })
        .context("Failed to bind the signaling port")?;

    if let Some(addr) = server.local_addr() {
        info!(%addr, "Signaling socket bound");
    }

    let (incoming_tx, incoming_rx) = mpsc::channel::<IncomingMessage>(32);
    let (decisions_tx, decisions_rx) = mpsc::unbounded_channel::<Decision>();

    let server_status = Arc::clone(&status);
    runtime.spawn(async move {
        if let Err(e) = server.run(incoming_tx).await {
            error!("Signaling server stopped: {e}");
            set_status(
                &server_status,
                Status::Failed {
                    reason: format!("Signaling server stopped: {e}"),
                },
            );
        }
    });

    let loop_config = config.clone();
    let loop_status = Arc::clone(&status);
    let loop_frame = Arc::clone(&frame);
    runtime.spawn(async move {
        SessionLoop::new(loop_config, loop_status, loop_frame)
            .run(incoming_rx, decisions_rx)
            .await;
    });

    info!(port = config.port, "Discovery and signaling started");

    Ok(NetHandle {
        _runtime: runtime,
        _advertiser: advertiser,
        status,
        frame,
        decisions: decisions_tx,
    })
}

/// Builds the TXT record senders read to decide whether they can cast here.
fn txt_record(config: &AppConfig, fingerprint: &str) -> TxtRecord {
    TxtRecord {
        version: PROTOCOL_VERSION,
        display_name: config.display_name.clone(),
        capabilities: String::new(),
        video_codecs: "h264".to_string(),
        audio_codecs: String::new(),
        // The receiver scales whatever it is sent to the window, so this
        // advertises the sender-facing ceiling from config rather than a
        // physical panel size.
        resolution: "1920x1080".to_string(),
        max_fps: config.framerate,
        fingerprint: fingerprint.to_string(),
        port: config.port,
    }
}

/// A sender that has asked to cast but has not yet been approved.
struct Pending {
    reply: ConnectionHandle,
    display_name: String,
    framerate: u32,
}

/// An approved sender, and the media session belonging to it.
struct Active {
    connection: ConnectionId,
    reply: ConnectionHandle,
    display_name: String,
    /// Present once an SDP offer has arrived and the pipeline is built.
    media: Option<Media>,
}

/// The GStreamer side of one accepted session.
struct Media {
    pipeline: ReceiverPipeline,
    peer: WebRtcPeer,
}

/// Owns all session state and processes one message at a time.
struct SessionLoop {
    config: AppConfig,
    status: SharedStatus,
    frame: SharedFrame,
    pending: HashMap<ConnectionId, Pending>,
    active: Option<Active>,
}

impl SessionLoop {
    fn new(config: AppConfig, status: SharedStatus, frame: SharedFrame) -> Self {
        Self {
            config,
            status,
            frame,
            pending: HashMap::new(),
            active: None,
        }
    }

    async fn run(
        mut self,
        mut incoming: mpsc::Receiver<IncomingMessage>,
        mut decisions: mpsc::UnboundedReceiver<Decision>,
    ) {
        // WebRTC events arrive from GStreamer threads on this channel and are
        // forwarded to whichever connection owns the current session.
        let (webrtc_tx, mut webrtc_rx) = mpsc::unbounded_channel::<WebRtcEvent>();

        // The transport reports a dropped connection only by closing the reply
        // channel, so a sender that loses power would otherwise leave the
        // receiver claiming to be connected forever.
        let mut liveness = tokio::time::interval(std::time::Duration::from_secs(2));
        liveness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(incoming) = incoming.recv() => {
                    self.handle_message(incoming, &webrtc_tx);
                }
                Some(decision) = decisions.recv() => {
                    self.handle_decision(decision);
                }
                Some(event) = webrtc_rx.recv() => {
                    self.handle_webrtc_event(event);
                }
                _ = liveness.tick() => {
                    self.drop_dead_connections();
                }
                else => break,
            }
        }

        debug!("Signaling message loop ended");
    }

    /// Answers one signaling message.
    fn handle_message(
        &mut self,
        incoming: IncomingMessage,
        webrtc_tx: &mpsc::UnboundedSender<WebRtcEvent>,
    ) {
        let IncomingMessage {
            message,
            reply,
            connection,
            peer,
        } = incoming;

        match message {
            SignalingMessage::SessionRequest {
                display_name,
                protocol_version,
                capabilities,
                ..
            } => self.handle_session_request(
                connection,
                peer,
                reply,
                display_name,
                protocol_version,
                capabilities,
            ),

            SignalingMessage::SdpOffer { sdp } => {
                self.handle_offer(connection, &sdp, webrtc_tx);
            }

            SignalingMessage::IceCandidate {
                candidate,
                sdp_mline_index,
                ..
            } => {
                let Some(media) = self.media_for(connection) else {
                    debug!(%connection, "Ignoring ICE for a connection with no session");
                    return;
                };
                media
                    .peer
                    .add_ice_candidate(sdp_mline_index.unwrap_or(0), &candidate);
            }

            SignalingMessage::IceComplete => {
                debug!(%connection, "Sender finished gathering ICE candidates");
            }

            SignalingMessage::Ping { timestamp_ms } => {
                reply.send(SignalingMessage::Pong {
                    timestamp_ms,
                    // A real clock reading, not an echo. Echoing the sender's
                    // value would make every clock-offset calculation come out
                    // as exactly -rtt/2 — a plausible-looking number that is
                    // always wrong, and that no test would catch.
                    receiver_timestamp_ms: now_millis(),
                });
            }

            SignalingMessage::SessionEnd { reason } => {
                // Only the connection that owns the session may end it.
                // Otherwise any peer on the network can stop someone's cast.
                if self.owns_session(connection) {
                    info!(?reason, %connection, "Sender ended the session");
                    self.tear_down();
                } else {
                    warn!(%connection, "Ignoring SessionEnd from a non-owning connection");
                }
                self.pending.remove(&connection);
            }

            other => {
                debug!(msg = ?std::mem::discriminant(&other), "Unhandled signaling message");
            }
        }
    }

    fn handle_session_request(
        &mut self,
        connection: ConnectionId,
        peer: SocketAddr,
        reply: ConnectionHandle,
        display_name: String,
        protocol_version: u32,
        capabilities: Capabilities,
    ) {
        if protocol_version != PROTOCOL_VERSION {
            warn!(
                sender = %display_name,
                theirs = protocol_version,
                ours = PROTOCOL_VERSION,
                "Rejecting session: protocol version mismatch"
            );
            reply.send(SignalingMessage::SessionReject {
                reason: RejectReason::VersionMismatch,
            });
            return;
        }

        if !capabilities.video_codecs.iter().any(|c| c == "h264") {
            warn!(sender = %display_name, "Rejecting session: no H.264 support");
            reply.send(SignalingMessage::SessionReject {
                reason: RejectReason::NoCompatibleCodecs,
            });
            return;
        }

        // One screen, one session. A second sender is told so rather than
        // silently replacing the first.
        if let Some(active) = &self.active {
            if active.connection != connection {
                warn!(sender = %display_name, "Rejecting session: already casting");
                reply.send(SignalingMessage::SessionReject {
                    reason: RejectReason::Busy,
                });
                return;
            }
        }

        let framerate = capabilities
            .max_framerate
            .unwrap_or(self.config.framerate)
            .min(self.config.framerate);

        info!(
            sender = %display_name,
            %peer,
            %connection,
            framerate,
            "Session requested — waiting for the user to approve it"
        );

        set_status(
            &self.status,
            Status::PendingConsent {
                sender_name: display_name.clone(),
                connection,
            },
        );

        self.pending.insert(
            connection,
            Pending {
                reply,
                display_name,
                framerate,
            },
        );
    }

    /// Applies the user's accept/deny answer to a pending request.
    fn handle_decision(&mut self, decision: Decision) {
        let Some(pending) = self.pending.remove(&decision.connection) else {
            debug!(
                connection = %decision.connection,
                "Consent answer for a request that is no longer pending"
            );
            return;
        };

        if !decision.accept {
            info!(sender = %pending.display_name, "User denied the cast");
            pending.reply.send(SignalingMessage::SessionReject {
                reason: RejectReason::Denied,
            });
            self.reset_status();
            return;
        }

        info!(sender = %pending.display_name, "User approved the cast");

        pending.reply.send(SignalingMessage::SessionAccept {
            receiver_id: self.config.display_name.clone(),
            negotiated: NegotiatedParams {
                video_codec: "h264".to_string(),
                audio_codec: None,
                max_bitrate_kbps: self.config.max_bitrate_kbps,
                framerate: pending.framerate,
            },
        });

        set_status(
            &self.status,
            Status::Negotiating {
                sender_name: pending.display_name.clone(),
            },
        );

        self.active = Some(Active {
            connection: decision.connection,
            reply: pending.reply,
            display_name: pending.display_name,
            media: None,
        });
    }

    /// Builds the media pipeline and answers the sender's offer.
    fn handle_offer(
        &mut self,
        connection: ConnectionId,
        sdp: &str,
        webrtc_tx: &mpsc::UnboundedSender<WebRtcEvent>,
    ) {
        let Some(active) = &mut self.active else {
            warn!(%connection, "Ignoring an SDP offer with no approved session");
            return;
        };
        if active.connection != connection {
            warn!(%connection, "Ignoring an SDP offer from a non-owning connection");
            return;
        }
        if active.media.is_some() {
            warn!(%connection, "Ignoring a second SDP offer — renegotiation is not supported");
            return;
        }

        let frame_slot = Arc::clone(&self.frame);
        let pipeline = match ReceiverPipeline::with_frame_handler(Arc::new(move |frame| {
            store_frame(&frame_slot, frame);
        })) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to build the receiver pipeline: {e:#}");
                self.fail(&format!("Could not start video: {e}"));
                return;
            }
        };

        if let Err(e) = pipeline.setup_bus_watch(log_bus_message) {
            warn!("Could not watch the receiver pipeline bus: {e:#}");
        }

        let peer = WebRtcPeer::new(pipeline.webrtcbin(), Role::Answerer, webrtc_tx.clone());

        // The pipeline must be playing before the answer is created, or
        // webrtcbin has no clock and produces no ICE candidates.
        if let Err(e) = pipeline.start() {
            error!("Failed to start the receiver pipeline: {e:#}");
            self.fail(&format!("Could not start video: {e}"));
            return;
        }

        if let Err(e) = peer.set_remote_description(SdpKind::Offer, sdp) {
            error!("Failed to apply the sender's offer: {e:#}");
            self.fail(&format!("Could not accept the video offer: {e}"));
            return;
        }

        info!(%connection, "SDP offer accepted — answering");
        active.media = Some(Media { pipeline, peer });
    }

    /// Forwards a WebRTC event to the sender, or reflects it in the UI.
    fn handle_webrtc_event(&mut self, event: WebRtcEvent) {
        let Some(active) = &self.active else {
            debug!("Dropping a WebRTC event with no active session");
            return;
        };

        match event {
            WebRtcEvent::LocalDescription { kind, sdp } => {
                if kind != SdpKind::Answer {
                    warn!(
                        ?kind,
                        "Receiver produced a description that is not an answer"
                    );
                    return;
                }
                active.reply.send(SignalingMessage::SdpAnswer { sdp });
            }
            WebRtcEvent::IceCandidate {
                sdp_mline_index,
                candidate,
            } => {
                active.reply.send(SignalingMessage::IceCandidate {
                    candidate,
                    sdp_mid: None,
                    sdp_mline_index: Some(sdp_mline_index),
                });
            }
            WebRtcEvent::IceGatheringComplete => {
                active.reply.send(SignalingMessage::IceComplete);
            }
            WebRtcEvent::Connected => {
                info!(sender = %active.display_name, "Media session connected");
                let sender_name = active.display_name.clone();
                set_status(&self.status, Status::Streaming { sender_name });
            }
            WebRtcEvent::Disconnected => {
                info!("Media session disconnected");
                self.tear_down();
            }
            WebRtcEvent::Failed(reason) => {
                error!(%reason, "Media session failed");
                self.fail(&reason);
            }
        }
    }

    /// Ends sessions and prompts whose sender has gone away.
    fn drop_dead_connections(&mut self) {
        self.pending.retain(|connection, pending| {
            let alive = pending.reply.is_connected();
            if !alive {
                info!(%connection, sender = %pending.display_name, "Pending sender disconnected");
            }
            alive
        });

        let active_dead = self
            .active
            .as_ref()
            .is_some_and(|a| !a.reply.is_connected());
        if active_dead {
            info!("Sender disconnected — ending the session");
            self.tear_down();
            return;
        }

        // A consent prompt whose sender vanished must not stay on screen: the
        // user would be approving a device that is no longer there, and the
        // next sender's prompt would be hidden behind it.
        let stale_prompt = match &*self.status.lock().unwrap_or_else(|e| e.into_inner()) {
            Status::PendingConsent { connection, .. } => !self.pending.contains_key(connection),
            _ => false,
        };
        if stale_prompt {
            self.reset_status();
        }
    }

    /// The media for `connection`, if it owns the active session.
    fn media_for(&self, connection: ConnectionId) -> Option<&Media> {
        let active = self.active.as_ref()?;
        (active.connection == connection).then_some(active.media.as_ref()?)
    }

    fn owns_session(&self, connection: ConnectionId) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| a.connection == connection)
    }

    /// Ends the session and returns to the waiting page.
    fn tear_down(&mut self) {
        if let Some(active) = self.active.take() {
            if let Some(media) = &active.media {
                if let Err(e) = media.pipeline.stop() {
                    warn!("Failed to stop the receiver pipeline: {e:#}");
                }
            }
            active.reply.send(SignalingMessage::SessionEnd {
                reason: SessionEndReason::UserStopped,
            });
        }
        clear_frame(&self.frame);
        self.reset_status();
    }

    /// Ends the session and shows why.
    fn fail(&mut self, reason: &str) {
        if let Some(active) = self.active.take() {
            if let Some(media) = &active.media {
                let _ = media.pipeline.stop();
            }
            active.reply.send(SignalingMessage::SessionEnd {
                reason: SessionEndReason::Error,
            });
        }
        clear_frame(&self.frame);
        set_status(
            &self.status,
            Status::Failed {
                reason: reason.to_string(),
            },
        );
    }

    /// Returns to `Waiting`, unless startup left a failure worth keeping.
    fn reset_status(&self) {
        set_status(&self.status, Status::Waiting);
    }
}

fn store_frame(slot: &SharedFrame, frame: VideoFrame) {
    match slot.lock() {
        Ok(mut guard) => {
            guard.frame = Some(frame);
            guard.sequence = guard.sequence.wrapping_add(1);
        }
        Err(e) => warn!("Frame slot poisoned, dropping frame: {e}"),
    }
}

fn clear_frame(slot: &SharedFrame) {
    if let Ok(mut guard) = slot.lock() {
        guard.frame = None;
        guard.sequence = guard.sequence.wrapping_add(1);
    }
}

/// Milliseconds since the Unix epoch, saturating at 0 if the clock is before it.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn set_status(status: &SharedStatus, next: Status) {
    match status.lock() {
        Ok(mut guard) => *guard = next,
        // A poisoned lock means a task panicked mid-update. The status is
        // cosmetic, so drop the update rather than take the window down.
        Err(e) => warn!("Status lock poisoned, dropping update: {e}"),
    }
}
