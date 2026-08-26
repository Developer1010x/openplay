//! The encrypted AirPlay 2 control channel.
//!
//! After a transient pair-setup completes at M4, the connection stops speaking
//! plaintext HTTP: the receiver closes it on the next unencrypted byte. Every
//! subsequent request and response is carried in ChaCha20-Poly1305 frames keyed
//! from the SRP session key.
//!
//! # Framing
//!
//! Each frame is a 2-byte little-endian plaintext length, then that many bytes
//! of ciphertext, then a 16-byte Poly1305 tag. The length prefix is *also* the
//! AEAD associated data, so a tampered length fails authentication rather than
//! desynchronising the stream.
//!
//! The nonce is a 64-bit little-endian counter zero-padded to 96 bits, counted
//! independently in each direction and never reset for the life of the
//! connection.

use anyhow::{anyhow, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hkdf::Hkdf;
use sha2::Sha512;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest plaintext one frame may carry, per the 2-byte length prefix.
const MAX_FRAME: usize = 0xFFFF;

/// HKDF salt shared by both control-channel keys.
const CONTROL_SALT: &[u8] = b"Control-Salt";
/// Info string for the key this side encrypts with.
const WRITE_INFO: &[u8] = b"Control-Write-Encryption-Key";
/// Info string for the key this side decrypts with.
const READ_INFO: &[u8] = b"Control-Read-Encryption-Key";

/// A `TcpStream` carrying encrypted control-channel frames.
pub struct ControlChannel {
    stream: TcpStream,
    write_cipher: ChaCha20Poly1305,
    read_cipher: ChaCha20Poly1305,
    write_counter: u64,
    read_counter: u64,
}

impl ControlChannel {
    /// Wraps a post-M4 connection, deriving both directional keys from `K`.
    pub fn new(stream: TcpStream, session_key: &[u8]) -> Result<Self> {
        Ok(Self {
            stream,
            write_cipher: cipher_from(session_key, WRITE_INFO)?,
            read_cipher: cipher_from(session_key, READ_INFO)?,
            write_counter: 0,
            read_counter: 0,
        })
    }

    /// Encrypts and sends one frame.
    pub async fn send(&mut self, plaintext: &[u8]) -> Result<()> {
        if plaintext.len() > MAX_FRAME {
            return Err(anyhow!(
                "control frame of {} bytes exceeds the {MAX_FRAME}-byte limit",
                plaintext.len()
            ));
        }

        let len = (plaintext.len() as u16).to_le_bytes();
        let sealed = self
            .write_cipher
            .encrypt(
                &nonce_for(self.write_counter),
                Payload {
                    msg: plaintext,
                    aad: &len,
                },
            )
            .map_err(|e| anyhow!("control frame encryption failed: {e}"))?;
        self.write_counter += 1;

        self.stream.write_all(&len).await?;
        self.stream.write_all(&sealed).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Reads and decrypts one frame.
    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 2];
        self.stream
            .read_exact(&mut len_buf)
            .await
            .context("control channel closed while reading a frame length")?;
        let len = u16::from_le_bytes(len_buf) as usize;

        // Ciphertext is the same length as the plaintext, plus the tag.
        let mut sealed = vec![0u8; len + 16];
        self.stream
            .read_exact(&mut sealed)
            .await
            .context("control channel closed mid-frame")?;

        let plaintext = self
            .read_cipher
            .decrypt(
                &nonce_for(self.read_counter),
                Payload {
                    msg: &sealed,
                    aad: &len_buf,
                },
            )
            .map_err(|e| anyhow!("control frame authentication failed: {e}"))?;
        self.read_counter += 1;

        Ok(plaintext)
    }

    /// Sends a request and reads frames until a complete HTTP response is held.
    pub async fn request(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.send(request).await?;

        let mut buf = Vec::new();
        loop {
            buf.extend_from_slice(&self.recv().await?);

            let Some(split) = find_header_end(&buf) else {
                continue;
            };
            let want = content_length(&buf[..split]).unwrap_or(0);
            if buf.len() - split >= want {
                return Ok(buf);
            }
        }
    }
}

/// Derives one directional key and builds its cipher.
fn cipher_from(session_key: &[u8], info: &[u8]) -> Result<ChaCha20Poly1305> {
    let hk = Hkdf::<Sha512>::new(Some(CONTROL_SALT), session_key);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key)
        .map_err(|e| anyhow!("control key derivation failed: {e}"))?;
    Ok(ChaCha20Poly1305::new(&key.into()))
}

/// 64-bit little-endian counter, zero-padded to the 96-bit nonce.
fn nonce_for(counter: u64) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    *Nonce::from_slice(&nonce)
}

/// Offset just past the blank line ending the headers.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Parses `Content-Length` from a header block.
fn content_length(headers: &[u8]) -> Option<usize> {
    let text = String::from_utf8_lossy(headers);
    text.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_a_little_endian_counter_in_the_high_eight_bytes() {
        assert_eq!(nonce_for(0).as_slice(), &[0u8; 12]);
        let n1 = nonce_for(1);
        assert_eq!(&n1.as_slice()[..4], &[0, 0, 0, 0], "leading padding");
        assert_eq!(&n1.as_slice()[4..], &1u64.to_le_bytes());
        assert_eq!(&nonce_for(258).as_slice()[4..], &258u64.to_le_bytes());
    }

    #[test]
    fn the_two_directional_keys_differ() {
        // Same session key, different info strings — deriving one key for both
        // directions would decrypt our own writes and never the peer's.
        let k = [7u8; 64];
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        Hkdf::<Sha512>::new(Some(CONTROL_SALT), &k)
            .expand(WRITE_INFO, &mut a)
            .unwrap();
        Hkdf::<Sha512>::new(Some(CONTROL_SALT), &k)
            .expand(READ_INFO, &mut b)
            .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn header_end_and_content_length_parse() {
        let msg = b"HTTP/1.1 200 OK\r\nContent-Length: 42\r\n\r\nbody";
        let split = find_header_end(msg).unwrap();
        assert_eq!(&msg[split..], b"body");
        assert_eq!(content_length(&msg[..split]), Some(42));
    }

    #[test]
    fn content_length_is_case_insensitive_and_optional() {
        let lower = b"HTTP/1.1 200 OK\r\ncontent-length: 7\r\n\r\n";
        let split = find_header_end(lower).unwrap();
        assert_eq!(content_length(&lower[..split]), Some(7));

        let none = b"HTTP/1.1 200 OK\r\nServer: AirTunes\r\n\r\n";
        let split = find_header_end(none).unwrap();
        assert_eq!(content_length(&none[..split]), None);
    }
}
