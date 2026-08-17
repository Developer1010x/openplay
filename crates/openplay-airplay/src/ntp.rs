use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tracing::{debug, error, info};

use crate::AirPlayError;

/// NTP epoch offset: seconds between 1900-01-01 and 1970-01-01.
const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;

/// NTP packet size.
const NTP_PACKET_SIZE: usize = 48;

/// Minimal NTP server for AirPlay timestamp synchronization.
///
/// AirPlay receivers query this NTP server to synchronize their clocks
/// with the sender. This is a stratum 1 server with reference ID "AIRP".
pub struct NtpServer {
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    local_addr: SocketAddr,
}

impl NtpServer {
    /// Starts the NTP server on the specified port.
    ///
    /// Binds to `0.0.0.0:port` and responds to NTP queries.
    pub async fn start(port: u16) -> Result<Self, AirPlayError> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).await.map_err(|e| {
            AirPlayError::Ntp(format!("Failed to bind NTP socket on port {port}: {e}"))
        })?;

        let local_addr = socket
            .local_addr()
            .map_err(|e| AirPlayError::Ntp(format!("Failed to get local addr: {e}")))?;

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let mut buf = [0u8; NTP_PACKET_SIZE];

            loop {
                tokio::select! {
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, addr)) => {
                                if len >= NTP_PACKET_SIZE {
                                    if let Some(response) = build_ntp_response(&buf) {
                                        if let Err(e) = socket.send_to(&response, addr).await {
                                            error!("Failed to send NTP response: {e}");
                                        } else {
                                            debug!(peer = %addr, "NTP response sent");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                error!("NTP recv error: {e}");
                                break;
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        info!("NTP server shutting down");
                        break;
                    }
                }
            }
        });

        info!(port, "AirPlay NTP server started");

        Ok(Self {
            shutdown_tx: Some(shutdown_tx),
            local_addr,
        })
    }

    /// Returns the local address the server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stops the NTP server.
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for NtpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Returns the current time as an NTP timestamp (seconds since 1900-01-01).
pub fn ntp_timestamp_now() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since_epoch.as_secs() + NTP_EPOCH_OFFSET;
    let frac = ((since_epoch.subsec_nanos() as u64) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

/// Builds an NTP response packet from a client request.
fn build_ntp_response(request: &[u8; NTP_PACKET_SIZE]) -> Option<[u8; NTP_PACKET_SIZE]> {
    let mut response = [0u8; NTP_PACKET_SIZE];

    // LI=0, VN=4, Mode=4 (server)
    let client_vn = (request[0] >> 3) & 0x07;
    response[0] = (client_vn << 3) | 4; // VN from client, mode=server

    // Stratum 1 (primary reference)
    response[1] = 1;

    // Poll interval (copy from client)
    response[2] = request[2];

    // Precision: ~1ms = -10 (2^-10 ≈ 0.001s)
    response[3] = (-10i8) as u8;

    // Root delay: 0
    // Root dispersion: 0
    // (bytes 4-11 already zero)

    // Reference ID: "AIRP" (4 ASCII bytes)
    response[12] = b'A';
    response[13] = b'I';
    response[14] = b'R';
    response[15] = b'P';

    let now = ntp_timestamp_now();
    let now_bytes = now.to_be_bytes();

    // Reference timestamp (bytes 16-23): last sync time = now
    response[16..24].copy_from_slice(&now_bytes);

    // Originate timestamp (bytes 24-31): copy client's transmit timestamp
    response[24..32].copy_from_slice(&request[40..48]);

    // Receive timestamp (bytes 32-39): when we received the request = now
    response[32..40].copy_from_slice(&now_bytes);

    // Transmit timestamp (bytes 40-47): when we send the response = now
    response[40..48].copy_from_slice(&now_bytes);

    Some(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ntp_timestamp_nonzero() {
        let ts = ntp_timestamp_now();
        assert!(ts > 0);
    }

    #[test]
    fn test_ntp_response_structure() {
        let mut request = [0u8; NTP_PACKET_SIZE];
        // Client: VN=4, Mode=3 (client)
        request[0] = (4 << 3) | 3;
        // Set a transmit timestamp
        request[40..48].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);

        let response = build_ntp_response(&request).unwrap();

        // VN=4, Mode=4 (server)
        assert_eq!(response[0] & 0x07, 4); // mode=server
        assert_eq!((response[0] >> 3) & 0x07, 4); // VN=4
                                                  // Stratum 1
        assert_eq!(response[1], 1);
        // Reference ID = "AIRP"
        assert_eq!(&response[12..16], b"AIRP");
        // Originate = client's transmit
        assert_eq!(
            &response[24..32],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }
}
