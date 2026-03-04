use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use openplay_protocol::SignalingMessage;
use rustls::ClientConfig;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use url::Url;

use crate::SignalingError;

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
    ) -> Result<(mpsc::Sender<SignalingMessage>, mpsc::Receiver<SignalingMessage>)> {
        info!(url = %self.url, "Connecting to receiver");

        let connector =
            tokio_tungstenite::Connector::Rustls(self.tls_config.clone());

        let (ws_stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
            self.url.as_str(),
            None,
            false,
            Some(connector),
        )
        .await
        .map_err(|e| SignalingError::Connection(format!("WebSocket connect failed: {e}")))?;

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
                if let Err(e) = ws_sink.send(Message::Text(json.into())).await {
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
