use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use openplay_protocol::SignalingMessage;
use rustls::ServerConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::SignalingError;

/// Embedded WebSocket signaling server for the receiver.
///
/// Listens for incoming TLS WebSocket connections from senders.
pub struct SignalingServer {
    addr: SocketAddr,
    tls_config: Arc<ServerConfig>,
}

impl SignalingServer {
    /// Creates a new signaling server.
    pub fn new(addr: SocketAddr, tls_config: Arc<ServerConfig>) -> Self {
        Self { addr, tls_config }
    }

    /// Starts the server and returns a channel for incoming messages.
    ///
    /// The server runs until the returned sender handle is dropped.
    pub async fn run(
        self,
        incoming_tx: mpsc::Sender<(SignalingMessage, mpsc::Sender<SignalingMessage>)>,
    ) -> Result<()> {
        let listener = TcpListener::bind(self.addr)
            .await
            .map_err(|e| SignalingError::Bind(format!("Failed to bind {}: {e}", self.addr)))?;

        info!(addr = %self.addr, "Signaling server listening");

        let tls_acceptor = tokio_rustls::TlsAcceptor::from(self.tls_config.clone());

        loop {
            let (tcp_stream, peer_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!("Failed to accept TCP connection: {e}");
                    continue;
                }
            };

            info!(peer = %peer_addr, "Incoming connection");

            let tls_acceptor = tls_acceptor.clone();
            let incoming_tx = incoming_tx.clone();

            tokio::spawn(async move {
                // TLS handshake
                let tls_stream = match tls_acceptor.accept(tcp_stream).await {
                    Ok(stream) => stream,
                    Err(e) => {
                        warn!(peer = %peer_addr, "TLS handshake failed: {e}");
                        return;
                    }
                };

                // WebSocket upgrade over the TLS stream
                let ws_stream = match tokio_tungstenite::accept_async(tls_stream).await {
                    Ok(ws) => ws,
                    Err(e) => {
                        warn!(peer = %peer_addr, "WebSocket upgrade failed: {e}");
                        return;
                    }
                };

                info!(peer = %peer_addr, "WebSocket connection established");

                // Create a channel for sending responses back to this client
                let (response_tx, mut response_rx) = mpsc::channel::<SignalingMessage>(32);

                let (mut ws_sink, mut ws_stream_read) = ws_stream.split();

                // Spawn a task to forward outgoing messages
                let send_task = tokio::spawn(async move {
                    while let Some(msg) = response_rx.recv().await {
                        let json = match serde_json::to_string(&msg) {
                            Ok(j) => j,
                            Err(e) => {
                                error!("Failed to serialize message: {e}");
                                continue;
                            }
                        };
                        if let Err(e) = ws_sink.send(Message::Text(json)).await {
                            error!(peer = %peer_addr, "Failed to send message: {e}");
                            break;
                        }
                    }
                });

                // Read incoming messages
                while let Some(result) = ws_stream_read.next().await {
                    match result {
                        Ok(Message::Text(text)) => {
                            match serde_json::from_str::<SignalingMessage>(&text) {
                                Ok(msg) => {
                                    debug!(peer = %peer_addr, msg_type = ?std::mem::discriminant(&msg), "Received message");
                                    if incoming_tx.send((msg, response_tx.clone())).await.is_err() {
                                        debug!("Incoming message channel closed");
                                        break;
                                    }
                                }
                                Err(e) => {
                                    warn!(peer = %peer_addr, "Invalid message: {e}");
                                }
                            }
                        }
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
                info!(peer = %peer_addr, "Connection closed");
            });
        }
    }
}
