use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tracing::debug;

use crate::mirror_header::{MirrorHeader, PacketType};
use crate::ntp::ntp_timestamp_now;
use crate::AirPlayError;

/// Writes AirPlay mirror packets (header + payload) to a TCP stream.
///
/// After HTTP negotiation, the TCP connection becomes a raw binary stream.
/// Each packet is: 128-byte header + variable-length payload.
pub struct MirrorStream {
    stream: TcpStream,
}

impl MirrorStream {
    /// Wraps an already-negotiated TCP stream.
    pub fn new(stream: TcpStream) -> Self {
        Self { stream }
    }

    /// Sends codec configuration data (SPS/PPS NALUs).
    ///
    /// This must be sent before any video frames.
    pub async fn send_codec_data(&mut self, sps_pps: &[u8]) -> Result<(), AirPlayError> {
        let header = MirrorHeader::new(
            PacketType::CodecData,
            sps_pps.len() as u32,
            ntp_timestamp_now(),
        );

        self.write_packet(&header, sps_pps).await?;
        debug!(len = sps_pps.len(), "Sent codec data");
        Ok(())
    }

    /// Sends an H.264 video frame.
    pub async fn send_video_frame(&mut self, nalu_data: &[u8]) -> Result<(), AirPlayError> {
        let header = MirrorHeader::new(
            PacketType::Video,
            nalu_data.len() as u32,
            ntp_timestamp_now(),
        );

        self.write_packet(&header, nalu_data).await?;
        debug!(len = nalu_data.len(), "Sent video frame");
        Ok(())
    }

    /// Sends a heartbeat packet (no payload).
    pub async fn send_heartbeat(&mut self) -> Result<(), AirPlayError> {
        let header = MirrorHeader::new(PacketType::Heartbeat, 0, ntp_timestamp_now());

        self.write_packet(&header, &[]).await?;
        debug!("Sent heartbeat");
        Ok(())
    }

    /// Writes a header + payload to the stream.
    async fn write_packet(
        &mut self,
        header: &MirrorHeader,
        payload: &[u8],
    ) -> Result<(), AirPlayError> {
        let header_bytes = header.encode();

        self.stream
            .write_all(&header_bytes)
            .await
            .map_err(|e| AirPlayError::MirrorStream(format!("Failed to write header: {e}")))?;

        if !payload.is_empty() {
            self.stream
                .write_all(payload)
                .await
                .map_err(|e| AirPlayError::MirrorStream(format!("Failed to write payload: {e}")))?;
        }

        self.stream
            .flush()
            .await
            .map_err(|e| AirPlayError::MirrorStream(format!("Flush failed: {e}")))?;

        Ok(())
    }

    /// Returns a reference to the inner stream.
    pub fn inner(&self) -> &TcpStream {
        &self.stream
    }

    /// Consumes the writer and returns the inner stream.
    pub fn into_inner(self) -> TcpStream {
        self.stream
    }
}
