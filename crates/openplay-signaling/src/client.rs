use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use openplay_protocol::SignalingMessage;
use rustls::ClientConfig;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use url::Url;

use crate::SignalingError;

/// Largest signaling frame accepted from a peer.
///
/// The same limit the server applies, for the same reason and against the same
/// class of peer. Dialling a receiver does not make it trusted: a hostile one
/// on the LAN is the better attack position of the two, because the sender is
/// the machine with a screen worth capturing. Tungstenite's default ceiling is
/// 64 MiB, which the peer can use to make this end allocate — twice, since
/// serde parses the text into owned `String` fields on top of the raw buffer.
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// How long the receiver may take to complete TLS and the WebSocket upgrade.
///
/// Without this, a receiver that accepts the socket and then stalls parks the
/// cast forever with no error for the UI to report — which any peer on the LAN
/// can arrange, since the sender dials whatever mDNS advertised.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// WebSocket signaling client for the sender.
///
/// Connects to a receiver's signaling server over TLS WebSocket.
pub struct SignalingClient {
    url: Url,
    tls_config: Arc<ClientConfig>,
}

impl SignalingClient {
    /// Creates a new signaling client targeting the given receiver URL.
    ///
    /// URL format: `wss://RECEIVER_IP:PORT`
    pub fn new(url: Url, tls_config: Arc<ClientConfig>) -> Self {
        Self { url, tls_config }
    }

    /// Connects to the receiver and returns channels for bidirectional messaging.
    ///
    /// Returns `(outgoing_sender, incoming_receiver)`:
    /// - Send `SignalingMessage` through `outgoing_sender` to transmit to the receiver.
    /// - Receive `SignalingMessage` from `incoming_receiver` for messages from the receiver.
    pub async fn connect(
        self,
    ) -> Result<(
        mpsc::Sender<SignalingMessage>,
        mpsc::Receiver<SignalingMessage>,
    )> {
        info!(url = %self.url, "Connecting to receiver");

        let connector = tokio_tungstenite::Connector::Rustls(self.tls_config.clone());

        let ws_config = WebSocketConfig {
            max_message_size: Some(MAX_MESSAGE_BYTES),
            max_frame_size: Some(MAX_MESSAGE_BYTES),
            ..Default::default()
        };

        let connecting = tokio_tungstenite::connect_async_tls_with_config(
            self.url.as_str(),
            Some(ws_config),
            false,
            Some(connector),
        );

        let (ws_stream, _response) = match tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting).await
        {
            Ok(Ok(connected)) => connected,
            Ok(Err(e)) => {
                return Err(
                    SignalingError::Connection(format!("WebSocket connect failed: {e}")).into(),
                )
            }
            Err(_) => {
                warn!(url = %self.url, "Receiver did not complete the handshake in time");
                return Err(SignalingError::Timeout.into());
            }
        };

        info!("Connected to receiver");

        let (mut ws_sink, mut ws_read) = ws_stream.split();

        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<SignalingMessage>(32);
        let (incoming_tx, incoming_rx) = mpsc::channel::<SignalingMessage>(32);

        // Task: forward outgoing messages to WebSocket
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                let json = match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize message: {e}");
                        continue;
                    }
                };
                if let Err(e) = ws_sink.send(Message::Text(json)).await {
                    error!("Failed to send message: {e}");
                    break;
                }
            }
        });

        // Task: read incoming messages from WebSocket
        tokio::spawn(async move {
            while let Some(result) = ws_read.next().await {
                match result {
                    Ok(Message::Text(text)) => {
                        match serde_json::from_str::<SignalingMessage>(&text) {
                            Ok(msg) => {
                                // The same gate the server applies to the
                                // sender. Deserialising only proves the JSON
                                // fits the shape; `validate` is what enforces
                                // the documented bounds before the message
                                // reaches a state machine or the UI.
                                if let Err(e) = msg.validate() {
                                    warn!("Rejecting message from receiver: {e}");
                                    continue;
                                }
                                debug!(msg_type = ?std::mem::discriminant(&msg), "Received message");
                                if incoming_tx.send(msg).await.is_err() {
                                    debug!("Incoming channel closed");
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!("Invalid message from receiver: {e}");
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        info!("Receiver closed connection");
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("WebSocket error: {e}");
                        break;
                    }
                }
            }
        });

        Ok((outgoing_tx, incoming_rx))
    }
}
