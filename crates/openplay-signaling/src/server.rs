use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use openplay_protocol::SignalingMessage;
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Semaphore};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::SignalingError;

/// Largest signaling frame accepted from a peer.
///
/// The biggest legitimate message is an SDP offer, a few kilobytes at most.
/// Tungstenite's default ceiling is 64 MiB, which an unauthenticated peer can
/// use to make the receiver allocate — twice, since serde parses the text into
/// owned `String` fields on top of the raw buffer.
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// How long a peer may take to complete TLS and the WebSocket upgrade.
///
/// Without this a peer that opens a socket and sends one byte holds a task, a
/// file descriptor and a TLS buffer indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an established connection may stay silent before it is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Concurrent connections accepted before new ones are refused.
///
/// A receiver serves one screen, so this only needs to be large enough for a
/// handful of senders browsing at once.
const MAX_CONNECTIONS: usize = 16;

/// How long to wait after an `accept` error before trying again.
///
/// On `EMFILE` the error repeats immediately, so retrying without a pause spins
/// a core and floods the log for as long as the condition lasts.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Identifies one sender connection for the life of that connection.
///
/// Session state — pairing, an accepted cast, a peer connection — belongs to a
/// connection, not to the receiver as a whole. Without an identity the
/// receiver cannot tell two senders apart, which lets any peer on the network
/// end another peer's session or relabel it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConnectionId(u64);

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

/// Sends messages back to one connected sender.
///
/// [`Self::send`] is deliberately non-blocking. The receiver drives every
/// connection from a single loop, so awaiting a full reply queue there lets one
/// peer that stops reading its socket freeze signaling for every other peer —
/// permanently, and at a cost of a few kilobytes.
#[derive(Debug, Clone)]
pub struct ConnectionHandle {
    id: ConnectionId,
    tx: mpsc::Sender<SignalingMessage>,
}

impl ConnectionHandle {
    /// The connection this handle writes to.
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    /// Queues a message. Returns `false` if the peer is gone or too far behind.
    ///
    /// A sender that cannot keep up with a queue of control messages is broken
    /// or hostile; dropping its message is the correct outcome either way.
    pub fn send(&self, message: SignalingMessage) -> bool {
        match self.tx.try_send(message) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(connection = %self.id, "Reply queue full — dropping message");
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                debug!(connection = %self.id, "Reply channel closed");
                false
            }
        }
    }

    /// Whether the peer is still connected.
    pub fn is_connected(&self) -> bool {
        !self.tx.is_closed()
    }
}

/// One message from one sender, with everything needed to answer it.
#[derive(Debug)]
pub struct IncomingMessage {
    pub message: SignalingMessage,
    /// Reply channel for the connection that sent it. Clone and keep this to
    /// push an SDP answer or trickle ICE later.
    pub reply: ConnectionHandle,
    pub connection: ConnectionId,
    pub peer: SocketAddr,
}

/// Embedded WebSocket signaling server for the receiver.
///
/// Listens for incoming TLS WebSocket connections from senders.
pub struct SignalingServer {
    listener: TcpListener,
    tls_config: Arc<ServerConfig>,
}

impl SignalingServer {
    /// Binds the listening socket.
    ///
    /// Binding here rather than inside [`Self::run`] means a caller learns
    /// about a port clash before it tells the user the receiver has started —
    /// and can read back the real port when it asked for port 0.
    pub async fn bind(addr: SocketAddr, tls_config: Arc<ServerConfig>) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| SignalingError::Bind(format!("Failed to bind {addr}: {e}")))?;

        Ok(Self {
            listener,
            tls_config,
        })
    }

    /// The address actually bound, which differs from the requested one when
    /// port 0 was asked for.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.listener.local_addr().ok()
    }

    /// Starts the server and feeds incoming messages to `incoming_tx`.
    ///
    /// Returns once `incoming_tx` is closed, so dropping the receiving half is
    /// how a caller shuts the server down.
    pub async fn run(self, incoming_tx: mpsc::Sender<IncomingMessage>) -> Result<()> {
        let listener = self.listener;
        let local_addr = listener
            .local_addr()
            .map_err(|e| SignalingError::Bind(format!("Bound socket has no address: {e}")))?;
        info!(addr = %local_addr, "Signaling server listening");

        let tls_acceptor = tokio_rustls::TlsAcceptor::from(self.tls_config.clone());
        let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let next_id = Arc::new(AtomicU64::new(1));

        loop {
            let accepted = tokio::select! {
                // Closing the receiving half is the shutdown signal, so stop
                // accepting rather than completing TLS handshakes for peers
                // whose messages can no longer go anywhere.
                _ = incoming_tx.closed() => {
                    info!("Incoming channel closed — signaling server stopping");
                    return Ok(());
                }
                accepted = listener.accept() => accepted,
            };

            let (tcp_stream, peer_addr) = match accepted {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("Failed to accept TCP connection: {e}");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                    continue;
                }
            };

            // Refuse rather than queue: an unbounded backlog of half-open
            // connections is the cheapest denial of service against a receiver.
            let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                warn!(peer = %peer_addr, "Connection limit reached — refusing");
                drop(tcp_stream);
                continue;
            };

            let id = ConnectionId(next_id.fetch_add(1, Ordering::Relaxed));
            info!(peer = %peer_addr, connection = %id, "Incoming connection");

            let tls_acceptor = tls_acceptor.clone();
            let incoming_tx = incoming_tx.clone();

            tokio::spawn(async move {
                // The permit is released when this task ends, however it ends.
                let _permit = permit;
                serve_connection(tls_acceptor, tcp_stream, peer_addr, id, incoming_tx).await;
            });
        }
    }
}

/// Runs one accepted connection to completion.
async fn serve_connection(
    tls_acceptor: tokio_rustls::TlsAcceptor,
    tcp_stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    id: ConnectionId,
    incoming_tx: mpsc::Sender<IncomingMessage>,
) {
    let tls_stream =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls_acceptor.accept(tcp_stream)).await {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => {
                warn!(peer = %peer_addr, "TLS handshake failed: {e}");
                return;
            }
            Err(_) => {
                warn!(peer = %peer_addr, "TLS handshake timed out");
                return;
            }
        };

    let ws_config = WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE_BYTES),
        max_frame_size: Some(MAX_MESSAGE_BYTES),
        ..Default::default()
    };

    let ws_stream = match tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::accept_async_with_config(tls_stream, Some(ws_config)),
    )
    .await
    {
        Ok(Ok(ws)) => ws,
        Ok(Err(e)) => {
            warn!(peer = %peer_addr, "WebSocket upgrade failed: {e}");
            return;
        }
        Err(_) => {
            warn!(peer = %peer_addr, "WebSocket upgrade timed out");
            return;
        }
    };

    info!(peer = %peer_addr, connection = %id, "WebSocket connection established");

    let (response_tx, mut response_rx) = mpsc::channel::<SignalingMessage>(32);
    let handle = ConnectionHandle {
        id,
        tx: response_tx,
    };

    let (mut ws_sink, mut ws_stream_read) = ws_stream.split();

    let send_task = tokio::spawn(async move {
        while let Some(msg) = response_rx.recv().await {
            let json = match serde_json::to_string(&msg) {
                Ok(j) => j,
                Err(e) => {
                    error!("Failed to serialize message: {e}");
                    continue;
                }
            };
            // A peer that has stopped reading must not park this task forever.
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, ws_sink.send(Message::Text(json))).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    error!(peer = %peer_addr, "Failed to send message: {e}");
                    break;
                }
                Err(_) => {
                    warn!(peer = %peer_addr, "Send timed out — closing connection");
                    break;
                }
            }
        }
    });

    loop {
        let next = match tokio::time::timeout(IDLE_TIMEOUT, ws_stream_read.next()).await {
            Ok(Some(result)) => result,
            Ok(None) => break,
            Err(_) => {
                info!(peer = %peer_addr, "Connection idle — closing");
                break;
            }
        };

        match next {
            Ok(Message::Text(text)) => match serde_json::from_str::<SignalingMessage>(&text) {
                Ok(message) => {
                    // Reject oversized or malformed content before it reaches
                    // any state machine or the UI.
                    if let Err(e) = message.validate() {
                        warn!(peer = %peer_addr, "Rejecting message: {e}");
                        continue;
                    }
                    debug!(
                        peer = %peer_addr,
                        msg_type = ?std::mem::discriminant(&message),
                        "Received message"
                    );
                    let incoming = IncomingMessage {
                        message,
                        reply: handle.clone(),
                        connection: id,
                        peer: peer_addr,
                    };
                    if incoming_tx.send(incoming).await.is_err() {
                        debug!("Incoming message channel closed");
                        break;
                    }
                }
                Err(e) => {
                    warn!(peer = %peer_addr, "Invalid message: {e}");
                }
            },
            Ok(Message::Close(_)) => {
                info!(peer = %peer_addr, "Client disconnected");
                break;
            }
            Ok(_) => {} // Ignore binary/ping/pong
            Err(e) => {
                warn!(peer = %peer_addr, "WebSocket error: {e}");
                break;
            }
        }
    }

    send_task.abort();
    info!(peer = %peer_addr, connection = %id, "Connection closed");
}
