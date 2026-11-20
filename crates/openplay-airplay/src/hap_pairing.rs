//! HomeKit Accessory Protocol (HAP) pairing for AirPlay.
//!
//! # Status
//!
//! The SRP-6a group this module uses was fabricated in an earlier revision and
//! has been replaced with the real RFC 5054 appendix A 3072-bit group; see
//! [`crate::srp`], whose tests re-derive it from RFC 3526's formula and check
//! the client against an independent implementation of the server side.
//!
//! That removes the known blocker to pairing with real hardware. It has not
//! been confirmed against physical Apple hardware, so treat pairing as
//! untested-in-the-field rather than proven.
//!
//! Note that a receiver requiring FairPlay will still fail later in the
//! session, for an unrelated reason — see [`crate::fairplay`].
//!
//! Implements:
//! - `pair-setup`: First-time pairing using SRP-6a with a 4-digit PIN
//! - `pair-verify`: Subsequent connections using stored Ed25519 keys
//!
//! Reference: Apple HomeKit Accessory Protocol Specification (non-commercial)

use std::net::SocketAddr;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha512;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey};

use crate::srp;
use crate::tlv8;

/// AirPlay pairing username (always "Pair-Setup" for pair-setup).
const PAIR_SETUP_USERNAME: &str = "Pair-Setup";

/// Result of a successful pair-setup: the device's long-term Ed25519 key
/// and the accessory's long-term public key.
#[derive(Debug, Clone)]
pub struct PairSetupResult {
    /// Our (client) Ed25519 signing key.
    pub client_ltsk: [u8; 32],
    /// Our (client) Ed25519 public key.
    pub client_ltpk: [u8; 32],
    /// The `iOSDevicePairingID` we sent in M5, under which the accessory has
    /// filed this pairing.
    ///
    /// Pair-verify M3 must send *this* back as `kTLVType_Identifier` and sign
    /// over it. It used to be generated inside pair-setup and dropped on the
    /// floor, leaving callers to substitute [`Self::accessory_id`]; the
    /// accessory then looked for a pairing filed under its own identifier,
    /// found none, and failed M4 with `kTLVError_Authentication` — which reads
    /// like a key mismatch rather than a lookup miss.
    pub client_pairing_id: String,
    /// Accessory's Ed25519 public key.
    pub accessory_ltpk: [u8; 32],
    /// Accessory device identifier (`AccessoryPairingID` from M6).
    pub accessory_id: String,
}

/// Result of a successful pair-verify: the shared encryption key for the session.
#[derive(Debug, Clone)]
pub struct PairVerifyResult {
    /// Shared session encryption key (derived from ECDH).
    pub shared_key: [u8; 32],
}

/// Stored pairing information for a device.
///
/// Two identifiers, and they are not interchangeable: [`Self::device_id`] names
/// the *accessory* and is what a pairing is looked up by here, while
/// [`Self::client_pairing_id`] names *us* and is what pair-verify puts on the
/// wire. Sending the accessory's identifier in M3 is a pairing that cannot be
/// found, not a credential that is refused.
#[derive(Debug, Clone)]
pub struct PairedDevice {
    /// The accessory's identifier — the local storage key.
    pub device_id: String,
    /// The `iOSDevicePairingID` this pairing was established under. Sent as
    /// `kTLVType_Identifier` in pair-verify M3 and covered by its signature.
    pub client_pairing_id: String,
    pub accessory_ltpk: [u8; 32],
    pub client_ltsk: [u8; 32],
    pub client_ltpk: [u8; 32],
}

impl From<PairSetupResult> for PairedDevice {
    /// Assembles the record pair-verify needs from a completed pair-setup.
    ///
    /// Provided so the two identifiers cannot be crossed by hand at the call
    /// site, which is how the accessory's own identifier ended up in
    /// pair-verify M3.
    fn from(result: PairSetupResult) -> Self {
        Self {
            device_id: result.accessory_id,
            client_pairing_id: result.client_pairing_id,
            accessory_ltpk: result.accessory_ltpk,
            client_ltsk: result.client_ltsk,
            client_ltpk: result.client_ltpk,
        }
    }
}

/// `kTLVType_Flags` value marking a pair-setup as transient.
///
/// pyatv 0.18.0 `pyatv/auth/hap_tlv8.py` defines `Flags.TransientPairing =
/// 0x10`, and sends it as a single byte; the HAP specification's
/// `kPairingFlag_Transient` is the same bit in a uint32.
///
/// This was `0x02` — a value that appears in no reference implementation and
/// corresponds to no defined flag. Measurement against AirTunes/950.7.1 found
/// the receiver ignores the TLV either way (`0x02`, `0x10`, and omitting
/// `FLAGS` all return an identical M2), which is why the wrong value survived;
/// it is not a reason to keep a fabricated constant, and a receiver that *does*
/// read the flag would have seen a request that is not marked transient at all.
/// What that receiver required instead was the `X-Apple-HKP` header — see
/// [`HKP_TRANSIENT`].
const FLAG_TRANSIENT: u8 = 0x10;

/// Perform transient pair-setup with an AirPlay 2 receiver (no PIN required).
///
/// Transient pairing is used when the receiver allows "Everyone on the Same Network"
/// access. Uses SRP-6a with PIN "3939" (standard AirPlay transient code).
///
/// Returns a [`TransientSession`]: the SRP session key and the connection it
/// belongs to. There is no long-term identity and so no pair-verify.
pub async fn pair_setup_transient(addr: SocketAddr) -> anyhow::Result<TransientSession> {
    let (stream, session_key) = pair_setup_srp(addr, "3939", true).await?;
    info!("Transient pair-setup complete — session key established");
    Ok(TransientSession {
        session_key,
        stream,
    })
}

/// Outcome of a *transient* pair-setup.
///
/// Transient pairing ends at M4. There is no M5/M6 identity exchange, so no
/// long-term key pair exists, nothing is persisted, and there is no pair-verify
/// to perform — the SRP session key keys the encrypted channel directly.
///
/// Running M5 anyway is what used to happen here, and the receiver responded by
/// closing the connection immediately after a successful M4.
///
/// The key is only meaningful on the connection it was negotiated over, so the
/// stream is handed back rather than dropped. Everything sent on it from this
/// point must be encrypted; the receiver drops the connection on plaintext.
pub struct TransientSession {
    /// Shared SRP session key (`K`).
    pub session_key: Vec<u8>,
    /// The connection the key belongs to.
    pub stream: TcpStream,
}

/// Perform pair-setup with an AirPlay receiver using a 4-digit PIN.
///
/// This is the first-time pairing flow using SRP-6a.
pub async fn pair_setup(addr: SocketAddr, pin: &str) -> anyhow::Result<PairSetupResult> {
    let (stream, session_key) = pair_setup_srp(addr, pin, false).await?;
    pair_setup_identity_exchange(stream, &session_key).await
}

/// Runs pair-setup M1-M4 (the SRP half) and returns the connection plus the
/// negotiated session key. Both pairing modes share this; only the PIN flow
/// continues into M5/M6.
async fn pair_setup_srp(
    addr: SocketAddr,
    pin: &str,
    transient: bool,
) -> anyhow::Result<(TcpStream, Vec<u8>)> {
    let mut stream = TcpStream::connect(addr).await?;
    info!(%addr, transient, "Starting HAP pair-setup");

    // M1: Client → Server: State=1, Method=PairSetup, [Flags=Transient]
    let m1 = if transient {
        tlv8::encode(&[
            tlv8::item_u8(tlv8::tags::STATE, 1),
            tlv8::item_u8(tlv8::tags::METHOD, tlv8::methods::PAIR_SETUP),
            tlv8::item_u8(tlv8::tags::FLAGS, FLAG_TRANSIENT),
        ])
    } else {
        tlv8::encode(&[
            tlv8::item_u8(tlv8::tags::STATE, 1),
            tlv8::item_u8(tlv8::tags::METHOD, tlv8::methods::PAIR_SETUP),
        ])
    };
    send_pair_setup(&mut stream, &m1).await?;

    // M2: Server → Client: State=2, PublicKey=B, Salt=s
    let m2_data = recv_pair_setup(&mut stream).await?;
    let m2 = tlv8::decode(&m2_data)?;

    check_error(&m2)?;
    check_state(&m2, 2)?;

    let server_pk_bytes = tlv8::lookup(&m2, tlv8::tags::PUBLIC_KEY)
        .ok_or_else(|| anyhow::anyhow!("M2: missing server public key"))?;
    let salt =
        tlv8::lookup(&m2, tlv8::tags::SALT).ok_or_else(|| anyhow::anyhow!("M2: missing salt"))?;

    debug!(
        server_pk_len = server_pk_bytes.len(),
        salt_len = salt.len(),
        "M2 received"
    );

    // SRP-6a client computation. The math and its tests live in `crate::srp`,
    // which cross-checks this against an independent implementation of the
    // server side.
    let a = srp::random_private_key();
    let client = srp::client_compute(PAIR_SETUP_USERNAME, pin, salt, server_pk_bytes, &a)?;
    let session_key = client.session_key;

    // M3: Client → Server: State=3, PublicKey=A, Proof=M1
    let m3 = tlv8::encode(&[
        tlv8::item_u8(tlv8::tags::STATE, 3),
        tlv8::item(tlv8::tags::PUBLIC_KEY, client.public_a.clone()),
        tlv8::item(tlv8::tags::PROOF, client.m1.clone()),
    ]);
    send_pair_setup(&mut stream, &m3).await?;

    // M4: Server → Client: State=4, Proof=M2
    let m4_data = recv_pair_setup(&mut stream).await?;
    let m4 = tlv8::decode(&m4_data)?;
    check_error(&m4)?;
    check_state(&m4, 4)?;

    let server_proof = tlv8::lookup(&m4, tlv8::tags::PROOF)
        .ok_or_else(|| anyhow::anyhow!("M4: missing server proof"))?;

    if server_proof != client.expected_m2.as_slice() {
        return Err(anyhow::anyhow!("Server proof verification failed"));
    }
    info!("SRP-6a verification successful");

    Ok((stream, session_key))
}

/// Pair-setup M5/M6: exchange and verify long-term identities.
///
/// PIN pairing only. Transient pairing has no identity to exchange and must not
/// reach this.
async fn pair_setup_identity_exchange(
    mut stream: TcpStream,
    session_key: &[u8],
) -> anyhow::Result<PairSetupResult> {
    // Derive encryption key for M5/M6 exchange
    let enc_key = hkdf_derive(
        b"Pair-Setup-Encrypt-Salt",
        session_key,
        b"Pair-Setup-Encrypt-Info",
        32,
    )?;

    // Generate our long-term Ed25519 key pair
    let client_ltsk = SigningKey::generate(&mut rand::thread_rng());
    let client_ltpk = client_ltsk.verifying_key();

    // Derive iOSDeviceX
    let device_x = hkdf_derive(
        b"Pair-Setup-Controller-Sign-Salt",
        session_key,
        b"Pair-Setup-Controller-Sign-Info",
        32,
    )?;

    // iOSDeviceInfo = iOSDeviceX || iOSDevicePairingID || iOSDeviceLTPK
    let client_pairing_id = uuid::Uuid::new_v4().to_string();
    let mut device_info = Vec::new();
    device_info.extend_from_slice(&device_x);
    device_info.extend_from_slice(client_pairing_id.as_bytes());
    device_info.extend_from_slice(client_ltpk.as_bytes());

    let device_sig = client_ltsk.sign(&device_info);

    // Encrypt sub-TLV with ChaCha20-Poly1305
    let sub_tlv = tlv8::encode(&[
        tlv8::item(
            tlv8::tags::IDENTIFIER,
            client_pairing_id.as_bytes().to_vec(),
        ),
        tlv8::item(tlv8::tags::PUBLIC_KEY, client_ltpk.as_bytes().to_vec()),
        tlv8::item(tlv8::tags::SIGNATURE, device_sig.to_bytes().to_vec()),
    ]);

    let enc_key_arr: [u8; 32] = enc_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("key length"))?;
    let cipher = ChaCha20Poly1305::new(&enc_key_arr.into());
    let encrypted = cipher
        .encrypt(&pairing_nonce(b"PS-Msg05"), sub_tlv.as_ref())
        .map_err(|e| anyhow::anyhow!("Encryption failed: {e}"))?;

    // M5: Client → Server: State=5, EncryptedData
    let m5 = tlv8::encode(&[
        tlv8::item_u8(tlv8::tags::STATE, 5),
        tlv8::item(tlv8::tags::ENCRYPTED_DATA, encrypted),
    ]);
    send_pair_setup(&mut stream, &m5).await?;

    // M6: Server → Client: State=6, EncryptedData
    let m6_data = recv_pair_setup(&mut stream).await?;
    let m6 = tlv8::decode(&m6_data)?;
    check_error(&m6)?;
    check_state(&m6, 6)?;

    let m6_encrypted = tlv8::lookup(&m6, tlv8::tags::ENCRYPTED_DATA)
        .ok_or_else(|| anyhow::anyhow!("M6: missing encrypted data"))?;

    let m6_decrypted = cipher
        .decrypt(&pairing_nonce(b"PS-Msg06"), m6_encrypted)
        .map_err(|e| anyhow::anyhow!("M6 decryption failed: {e}"))?;

    let m6_sub = tlv8::decode(&m6_decrypted)?;

    let accessory_id = tlv8::lookup(&m6_sub, tlv8::tags::IDENTIFIER)
        .ok_or_else(|| anyhow::anyhow!("M6: missing accessory ID"))?;
    let accessory_ltpk_bytes = tlv8::lookup(&m6_sub, tlv8::tags::PUBLIC_KEY)
        .ok_or_else(|| anyhow::anyhow!("M6: missing accessory LTPK"))?;
    let accessory_sig = tlv8::lookup(&m6_sub, tlv8::tags::SIGNATURE)
        .ok_or_else(|| anyhow::anyhow!("M6: missing accessory signature"))?;

    // Verify accessory signature
    let accessory_x = hkdf_derive(
        b"Pair-Setup-Accessory-Sign-Salt",
        session_key,
        b"Pair-Setup-Accessory-Sign-Info",
        32,
    )?;

    let mut accessory_info = Vec::new();
    accessory_info.extend_from_slice(&accessory_x);
    accessory_info.extend_from_slice(accessory_id);
    accessory_info.extend_from_slice(accessory_ltpk_bytes);

    let accessory_pk: [u8; 32] = accessory_ltpk_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid accessory LTPK length"))?;
    let accessory_verifying = VerifyingKey::from_bytes(&accessory_pk)?;
    let sig_bytes: [u8; 64] = accessory_sig
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid signature length"))?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    accessory_verifying.verify(&accessory_info, &signature)?;

    info!(
        accessory_id = %String::from_utf8_lossy(accessory_id),
        "Pair-setup completed successfully"
    );

    Ok(PairSetupResult {
        client_ltsk: client_ltsk.to_bytes(),
        client_ltpk: client_ltpk.to_bytes(),
        client_pairing_id,
        accessory_ltpk: accessory_pk,
        accessory_id: String::from_utf8_lossy(accessory_id).to_string(),
    })
}

/// Perform pair-verify with a previously paired AirPlay receiver.
///
/// This establishes a shared session key for encrypted communication.
pub async fn pair_verify(
    addr: SocketAddr,
    paired: &PairedDevice,
) -> anyhow::Result<(TcpStream, PairVerifyResult)> {
    let mut stream = TcpStream::connect(addr).await?;
    info!(%addr, "Starting HAP pair-verify");

    // Generate ephemeral X25519 key pair
    let client_secret = EphemeralSecret::random_from_rng(rand::thread_rng());
    let client_public = X25519PublicKey::from(&client_secret);

    // M1: Client → Server: State=1, PublicKey=clientEphemeralPK
    let m1 = tlv8::encode(&[
        tlv8::item_u8(tlv8::tags::STATE, 1),
        tlv8::item(tlv8::tags::PUBLIC_KEY, client_public.as_bytes().to_vec()),
    ]);
    send_pair_verify(&mut stream, &m1).await?;

    // M2: Server → Client: State=2, PublicKey=serverEphemeralPK, EncryptedData
    let m2_data = recv_pair_verify(&mut stream).await?;
    let m2 = tlv8::decode(&m2_data)?;
    check_error(&m2)?;
    check_state(&m2, 2)?;

    let server_epk_bytes = tlv8::lookup(&m2, tlv8::tags::PUBLIC_KEY)
        .ok_or_else(|| anyhow::anyhow!("M2: missing server ephemeral PK"))?;
    let m2_encrypted = tlv8::lookup(&m2, tlv8::tags::ENCRYPTED_DATA)
        .ok_or_else(|| anyhow::anyhow!("M2: missing encrypted data"))?;

    // Compute shared secret via X25519
    let server_epk: [u8; 32] = server_epk_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid server EPK length"))?;
    let server_public = X25519PublicKey::from(server_epk);
    let shared_secret = client_secret.diffie_hellman(&server_public);

    // Derive session encryption key
    let session_key = hkdf_derive(
        b"Pair-Verify-Encrypt-Salt",
        shared_secret.as_bytes(),
        b"Pair-Verify-Encrypt-Info",
        32,
    )?;

    let key_arr: [u8; 32] = session_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("key len"))?;
    let cipher = ChaCha20Poly1305::new(&key_arr.into());

    // Decrypt M2 encrypted data
    let m2_plain = cipher
        .decrypt(&pairing_nonce(b"PV-Msg02"), m2_encrypted)
        .map_err(|e| anyhow::anyhow!("M2 decryption failed: {e}"))?;

    let m2_sub = tlv8::decode(&m2_plain)?;

    let server_id = tlv8::lookup(&m2_sub, tlv8::tags::IDENTIFIER)
        .ok_or_else(|| anyhow::anyhow!("M2 sub-TLV: missing identifier"))?;
    let server_sig = tlv8::lookup(&m2_sub, tlv8::tags::SIGNATURE)
        .ok_or_else(|| anyhow::anyhow!("M2 sub-TLV: missing signature"))?;

    // Verify server signature
    let mut server_info = Vec::new();
    server_info.extend_from_slice(server_epk_bytes);
    server_info.extend_from_slice(server_id);
    server_info.extend_from_slice(client_public.as_bytes());

    let accessory_verifying = VerifyingKey::from_bytes(&paired.accessory_ltpk)?;
    let sig_bytes: [u8; 64] = server_sig
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid signature length"))?;
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    accessory_verifying.verify(&server_info, &signature)?;

    debug!("Server signature verified");

    // Build client proof over our *own* pairing identifier.
    let client_ltsk = SigningKey::from_bytes(&paired.client_ltsk);
    let sub_tlv = verify_m3_sub_tlv(
        &client_ltsk,
        &paired.client_pairing_id,
        client_public.as_bytes(),
        server_epk_bytes,
    );

    let encrypted = cipher
        .encrypt(&pairing_nonce(b"PV-Msg03"), sub_tlv.as_ref())
        .map_err(|e| anyhow::anyhow!("M3 encryption failed: {e}"))?;

    // M3: Client → Server: State=3, EncryptedData
    let m3 = tlv8::encode(&[
        tlv8::item_u8(tlv8::tags::STATE, 3),
        tlv8::item(tlv8::tags::ENCRYPTED_DATA, encrypted),
    ]);
    send_pair_verify(&mut stream, &m3).await?;

    // M4: Server → Client: State=4 (success)
    let m4_data = recv_pair_verify(&mut stream).await?;
    let m4 = tlv8::decode(&m4_data)?;
    check_error(&m4)?;
    check_state(&m4, 4)?;

    info!("Pair-verify completed successfully");

    Ok((
        stream,
        PairVerifyResult {
            shared_key: key_arr,
        },
    ))
}

/// Builds the pair-verify M3 sub-TLV: our pairing identifier plus a signature
/// over `iOSDeviceInfo`.
///
/// ```text
/// iOSDeviceInfo = iOSDeviceEphemeralPK || iOSDevicePairingID || AccessoryEphemeralPK
/// ```
///
/// `client_pairing_id` is the identifier *we* registered in pair-setup M5, and
/// is the accessory's lookup key for this pairing. Split out from
/// [`pair_verify`] so the choice of identifier is testable without a socket —
/// it was the accessory's own identifier here, which no accessory can resolve.
///
/// Matches pyatv 0.18.0 `pyatv/auth/hap_srp.py::SRPAuthHandler.verify1`, which
/// signs `public_key + self.pairing_id + atv_public_key` and sends
/// `{Identifier: self.pairing_id, Signature: signature}`.
fn verify_m3_sub_tlv(
    client_ltsk: &SigningKey,
    client_pairing_id: &str,
    client_epk: &[u8],
    accessory_epk: &[u8],
) -> Vec<u8> {
    let mut client_info = Vec::new();
    client_info.extend_from_slice(client_epk);
    client_info.extend_from_slice(client_pairing_id.as_bytes());
    client_info.extend_from_slice(accessory_epk);

    let client_sig = client_ltsk.sign(&client_info);

    tlv8::encode(&[
        tlv8::item(
            tlv8::tags::IDENTIFIER,
            client_pairing_id.as_bytes().to_vec(),
        ),
        tlv8::item(tlv8::tags::SIGNATURE, client_sig.to_bytes().to_vec()),
    ])
}

/// Expands one of HAP's 8-byte pairing nonces to the 96 bits
/// ChaCha20-Poly1305 wants, by padding with four zeros **in front**.
///
/// The label occupies the *low* eight bytes. Three independent confirmations:
///
/// - pyatv 0.18.0 `pyatv/support/chacha20.py`:
///   `return b"\x00" * (NONCE_LENGTH - len(nonce)) + nonce`
/// - HAP-python `pyhap/hap_crypto.py`: `nonce.rjust(12, b"\x00")`
/// - this crate's own [`crate::control_channel`], which writes its frame
///   counter to `nonce[4..]` and leaves `nonce[..4]` zero
///
/// These nonces used to be written as `b"PS-Msg05\x00\x00\x00\x00"` — the
/// padding on the wrong end, which is a different nonce and so a different
/// keystream. Nothing decrypts, and the failure appears at M6/M4 as an
/// authentication error. Only the PIN flow and pair-verify reach this code,
/// which is why hardware testing of transient pairing never exercised it.
fn pairing_nonce(label: &[u8; 8]) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(label);
    *Nonce::from_slice(&nonce)
}

// --- HTTP helpers for /pair-setup and /pair-verify ---

/// HomeKit pairing type, sent as `X-Apple-HKP`.
///
/// Without this header the receiver answers **400 Bad Request** before looking
/// at the body at all — it is how the endpoint selects a pairing flow, not an
/// optional hint. Verified against a Mac running AirTunes/950.7.1: with the
/// header, `/pair-setup` returns a real M2 (state, 16-byte salt, 384-byte
/// public key); without it, 400 every time, regardless of `Host` or the value
/// of the transient flag.
const HKP_TRANSIENT: u8 = 4;

async fn send_pair_setup(stream: &mut TcpStream, body: &[u8]) -> anyhow::Result<()> {
    send_post(stream, "/pair-setup", "application/octet-stream", body).await
}

async fn recv_pair_setup(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    recv_response(stream).await
}

async fn send_pair_verify(stream: &mut TcpStream, body: &[u8]) -> anyhow::Result<()> {
    send_post(stream, "/pair-verify", "application/octet-stream", body).await
}

async fn recv_pair_verify(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    recv_response(stream).await
}

async fn send_post(
    stream: &mut TcpStream,
    path: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let header = format!(
        "POST {path} HTTP/1.1\r\n\
         X-Apple-HKP: {HKP_TRANSIENT}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

async fn recv_response(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0u8; 8192];
    let mut total = 0;

    // Read HTTP response header + body
    loop {
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            return Err(anyhow::anyhow!("Connection closed"));
        }
        total += n;

        // Find end of headers
        if let Some(header_end) = find_header_end(&buf[..total]) {
            // Parse Content-Length
            let header_str = String::from_utf8_lossy(&buf[..header_end]);

            // Check the status line before touching the body. A receiver that
            // refuses the request answers with a non-2xx status and, usually,
            // an empty body — which would otherwise surface downstream as
            // "Missing state TLV" and read like a crypto failure. Observed
            // against macOS AirPlay Receiver, which answers 403 to every
            // endpoint except /info when its access setting does not admit
            // this device.
            check_http_status(&header_str)?;

            let content_length = parse_content_length(&header_str).unwrap_or(0);
            let body_start = header_end + 4; // after \r\n\r\n
            let body_received = total - body_start;

            if body_received >= content_length {
                return Ok(buf[body_start..body_start + content_length].to_vec());
            }

            // Need more body data
            if buf.len() < body_start + content_length {
                buf.resize(body_start + content_length, 0);
            }
            while total - body_start < content_length {
                let n = stream.read(&mut buf[total..]).await?;
                if n == 0 {
                    return Err(anyhow::anyhow!("Connection closed mid-body"));
                }
                total += n;
            }
            return Ok(buf[body_start..body_start + content_length].to_vec());
        }

        if total >= buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
    }
}

/// Rejects a non-2xx HTTP response with a message naming the actual status.
///
/// Without this, an HTTP-level refusal reaches the TLV8 decoder as an empty
/// body and is reported as a missing-TLV error, which reads like a protocol or
/// crypto fault and sends the reader in entirely the wrong direction.
///
/// 403 in particular is an access-policy answer, not a pairing failure: macOS
/// AirPlay Receiver returns it for every endpoint except `/info` when its
/// "AirPlay Receiver" setting does not admit the calling device — for example
/// when it is set to "Current User" and the caller is not signed into the same
/// Apple ID. No pairing credential can satisfy that; the setting has to change.
fn check_http_status(headers: &str) -> anyhow::Result<()> {
    if headers.lines().next().is_none() {
        anyhow::bail!("Empty HTTP response");
    }

    // "HTTP/1.1 403 Forbidden" → 403, read off the first line only.
    let code = match crate::http_session::status_code(headers) {
        Some(c) => c,
        // Not a status line we recognise; let the body parser decide.
        None => return Ok(()),
    };

    if (200..300).contains(&code) {
        return Ok(());
    }

    let hint = match code {
        403 => {
            " — the receiver refused this device. On macOS, check System Settings → \
                General → AirDrop & Handoff → AirPlay Receiver; \"Current User\" rejects \
                devices not signed into the same Apple ID. This is not a pairing failure"
        }
        470 | 401 => " — the receiver requires a password or PIN",
        500 => " — the receiver rejected the request body",
        _ => "",
    };

    Err(anyhow::anyhow!("Receiver returned HTTP {code}{hint}"))
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(header: &str) -> Option<usize> {
    for line in header.lines() {
        if let Some(value) = line
            .strip_prefix("Content-Length: ")
            .or_else(|| line.strip_prefix("content-length: "))
        {
            return value.trim().parse().ok();
        }
    }
    None
}

// --- TLV helpers ---

fn check_state(items: &[tlv8::Tlv8Item], expected: u8) -> anyhow::Result<()> {
    let state = tlv8::lookup(items, tlv8::tags::STATE)
        .and_then(|v| v.first().copied())
        .ok_or_else(|| anyhow::anyhow!("Missing state TLV"))?;
    if state != expected {
        return Err(anyhow::anyhow!(
            "Unexpected state: got {state}, expected {expected}"
        ));
    }
    Ok(())
}

/// The accessory answered `kTLVError_BackOff` (0x03).
///
/// This is a rate limit, not a credential problem: the accessory is telling us
/// to come back later, and until the delay elapses *every* attempt fails the
/// same way no matter what PIN or key is offered. Reported as its own error
/// type so a caller can tell the two apart rather than logging "authentication
/// failed" and sending the reader after the crypto — which is how a whole round
/// of hardware testing was invalidated (issue #27).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HapBackOff {
    /// Value of `kTLVType_RetryDelay`, when the accessory sent one.
    pub retry_after: Option<std::time::Duration>,
}

impl std::fmt::Display for HapBackOff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HAP back-off (kTLVError_BackOff 0x03): the receiver is rate-limiting pairing attempts"
        )?;
        match self.retry_after {
            Some(d) => write!(f, " and asked to retry after {} s", d.as_secs())?,
            None => write!(f, " and sent no kTLVType_RetryDelay")?,
        }
        write!(
            f,
            ". This is a retry-after, not an authentication failure — the PIN and keys are not implicated"
        )
    }
}

impl std::error::Error for HapBackOff {}

/// Reads `kTLVType_RetryDelay` (0x08).
///
/// HAP integer TLVs are little-endian and only as wide as they need to be, so
/// the delay can arrive as 1, 2, 4 or 8 bytes. pyatv 0.18.0
/// `pyatv/auth/hap_tlv8.py::stringify` reads the same TLV as
/// `int.from_bytes(value, byteorder="little")` and renders it as seconds.
fn retry_delay_secs(items: &[tlv8::Tlv8Item]) -> Option<u64> {
    let raw = tlv8::lookup(items, tlv8::tags::RETRY_DELAY)?;
    if raw.is_empty() || raw.len() > 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf[..raw.len()].copy_from_slice(raw);
    Some(u64::from_le_bytes(buf))
}

/// Turns a `kTLVType_Error` in a received message into an error, if it is one.
///
/// An ERROR TLV that is absent, empty, or carries `kTLVError_None` is *not* a
/// failure. This used to default a missing value to 0 and then report it, so a
/// zero-length ERROR TLV — which some accessories send alongside a perfectly
/// good message — became "HAP error 0: Unknown HAP error".
fn check_error(items: &[tlv8::Tlv8Item]) -> anyhow::Result<()> {
    let Some(err) = tlv8::lookup(items, tlv8::tags::ERROR) else {
        return Ok(());
    };
    // A zero-length ERROR TLV carries no code; treat it as no error rather than
    // inventing one.
    let Some(code) = err.first().copied() else {
        return Ok(());
    };
    if code == tlv8::errors::NONE {
        return Ok(());
    }

    if code == tlv8::errors::BACKOFF {
        return Err(HapBackOff {
            retry_after: retry_delay_secs(items).map(std::time::Duration::from_secs),
        }
        .into());
    }

    let msg = match code {
        tlv8::errors::UNKNOWN => "Unknown error",
        tlv8::errors::AUTHENTICATION => "Authentication failed",
        tlv8::errors::MAX_PEERS => "Maximum peers reached",
        tlv8::errors::MAX_TRIES => "Maximum tries reached",
        tlv8::errors::UNAVAILABLE => "Resource unavailable",
        tlv8::errors::BUSY => "Device busy",
        _ => "Unknown HAP error",
    };
    Err(anyhow::anyhow!("HAP error {code}: {msg}"))
}

// --- Crypto helpers ---

fn hkdf_derive(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> anyhow::Result<Vec<u8>> {
    let hk = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut okm = vec![0u8; len];
    hk.expand(info, &mut okm)
        .map_err(|e| anyhow::anyhow!("HKDF expand failed: {e}"))?;
    Ok(okm)
}

// --- Paired device storage ---

/// Initialize the paired devices SQLite database.
///
/// `client_pairing_id` is stored alongside the keys because pair-verify cannot
/// be performed without it; a row that has the keys but not the identifier
/// describes a pairing the accessory will not find.
pub fn init_paired_db(db_path: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS paired_devices (
            device_id TEXT PRIMARY KEY,
            client_pairing_id TEXT NOT NULL,
            accessory_ltpk BLOB NOT NULL,
            client_ltsk BLOB NOT NULL,
            client_ltpk BLOB NOT NULL,
            created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );",
    )?;
    Ok(conn)
}

/// Store a paired device.
pub fn store_paired_device(
    conn: &rusqlite::Connection,
    device: &PairedDevice,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO paired_devices
             (device_id, client_pairing_id, accessory_ltpk, client_ltsk, client_ltpk)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            device.device_id,
            device.client_pairing_id,
            device.accessory_ltpk.as_slice(),
            device.client_ltsk.as_slice(),
            device.client_ltpk.as_slice(),
        ],
    )?;
    Ok(())
}

/// Load a paired device by ID.
pub fn load_paired_device(
    conn: &rusqlite::Connection,
    device_id: &str,
) -> anyhow::Result<Option<PairedDevice>> {
    let mut stmt = conn.prepare(
        "SELECT device_id, client_pairing_id, accessory_ltpk, client_ltsk, client_ltpk
         FROM paired_devices WHERE device_id = ?1",
    )?;

    let mut rows = stmt.query(rusqlite::params![device_id])?;
    if let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let client_pairing_id: String = row.get(1)?;
        let ltpk: Vec<u8> = row.get(2)?;
        let ltsk: Vec<u8> = row.get(3)?;
        let ltpk_client: Vec<u8> = row.get(4)?;

        Ok(Some(PairedDevice {
            device_id: id,
            client_pairing_id,
            accessory_ltpk: ltpk
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid LTPK"))?,
            client_ltsk: ltsk
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid LTSK"))?,
            client_ltpk: ltpk_client
                .try_into()
                .map_err(|_| anyhow::anyhow!("Invalid client LTPK"))?,
        }))
    } else {
        Ok(None)
    }
}

/// List all paired device IDs.
pub fn list_paired_devices(conn: &rusqlite::Connection) -> anyhow::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT device_id FROM paired_devices")?;
    let ids = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_accepts_2xx() {
        assert!(check_http_status("HTTP/1.1 200 OK\r\nContent-Length: 9\r\n").is_ok());
        assert!(check_http_status("HTTP/1.1 204 No Content\r\n").is_ok());
    }

    #[test]
    fn http_status_rejects_403_with_an_actionable_message() {
        // Observed against macOS AirPlay Receiver set to "Current User".
        let err = check_http_status("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n")
            .expect_err("403 must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("403"), "should name the status: {msg}");
        assert!(
            msg.contains("AirPlay Receiver"),
            "should point at the setting that causes it: {msg}"
        );
        assert!(
            !msg.contains("TLV"),
            "must not read like a protocol/crypto fault: {msg}"
        );
    }

    #[test]
    fn http_status_rejects_other_failures() {
        assert!(check_http_status("HTTP/1.1 500 Internal Server Error\r\n").is_err());
        assert!(check_http_status("HTTP/1.1 470 Connection Authorization Required\r\n").is_err());
    }

    #[test]
    fn http_status_defers_when_there_is_no_status_line() {
        // Not our job to reject something we cannot parse — let the body parser try.
        assert!(check_http_status("garbage without a code\r\n").is_ok());
    }

    #[test]
    fn test_hkdf_derive() {
        let key = hkdf_derive(b"salt", b"ikm", b"info", 32).unwrap();
        assert_eq!(key.len(), 32);
        // Should be deterministic
        let key2 = hkdf_derive(b"salt", b"ikm", b"info", 32).unwrap();
        assert_eq!(key, key2);
    }

    #[test]
    fn test_paired_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_paired.db");
        let conn = init_paired_db(&db_path).unwrap();

        let device = PairedDevice {
            device_id: "test-device-001".into(),
            client_pairing_id: "0f1e2d3c-client".into(),
            accessory_ltpk: [1u8; 32],
            client_ltsk: [2u8; 32],
            client_ltpk: [3u8; 32],
        };

        store_paired_device(&conn, &device).unwrap();
        let loaded = load_paired_device(&conn, "test-device-001")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.device_id, "test-device-001");
        assert_eq!(loaded.accessory_ltpk, [1u8; 32]);
        assert_eq!(loaded.client_ltsk, [2u8; 32]);
        // Without this, pair-verify has nothing to send as kTLVType_Identifier
        // and falls back to the accessory's own id, which resolves to no pairing.
        assert_eq!(loaded.client_pairing_id, "0f1e2d3c-client");

        let ids = list_paired_devices(&conn).unwrap();
        assert_eq!(ids, vec!["test-device-001"]);
    }

    /// The four HAP pairing nonces, byte for byte.
    ///
    /// Reference: pyatv 0.18.0 `pyatv/support/chacha20.py`, which pads a short
    /// nonce as `b"\x00" * (NONCE_LENGTH - len(nonce)) + nonce` and calls
    /// `encrypt(..., nonce="PS-Msg05".encode())` in `auth/hap_srp.py`.
    /// HAP-python's `pyhap/hap_crypto.py` does `nonce.rjust(12, b"\x00")`.
    #[test]
    fn pairing_nonces_pad_in_front() {
        assert_eq!(
            pairing_nonce(b"PS-Msg05").as_slice(),
            b"\x00\x00\x00\x00PS-Msg05"
        );
        assert_eq!(
            pairing_nonce(b"PS-Msg06").as_slice(),
            b"\x00\x00\x00\x00PS-Msg06"
        );
        assert_eq!(
            pairing_nonce(b"PV-Msg02").as_slice(),
            b"\x00\x00\x00\x00PV-Msg02"
        );
        assert_eq!(
            pairing_nonce(b"PV-Msg03").as_slice(),
            b"\x00\x00\x00\x00PV-Msg03"
        );

        // Stated the other way round, because this is the shape the bug had:
        // trailing padding is a different nonce and therefore a different
        // keystream, and nothing on the far side decrypts.
        let n = pairing_nonce(b"PS-Msg05");
        assert_eq!(&n.as_slice()[..4], &[0, 0, 0, 0], "padding leads");
        assert_ne!(n.as_slice(), b"PS-Msg05\x00\x00\x00\x00");

        // Same layout the control channel already uses for its frame counter,
        // which is the in-repo contradiction that flagged this.
        assert_eq!(n.len(), 12);
    }

    #[test]
    fn a_pair_setup_result_keeps_the_two_identifiers_apart() {
        let paired: PairedDevice = PairSetupResult {
            client_ltsk: [2u8; 32],
            client_ltpk: [3u8; 32],
            client_pairing_id: "ours".into(),
            accessory_ltpk: [1u8; 32],
            accessory_id: "theirs".into(),
        }
        .into();

        assert_eq!(paired.client_pairing_id, "ours", "M3 sends our identifier");
        assert_eq!(
            paired.device_id, "theirs",
            "we file the pairing under theirs"
        );
    }

    /// Pair-verify M3 must name *us*, not the accessory.
    #[test]
    fn verify_m3_sends_the_client_pairing_id() {
        let client_ltsk = SigningKey::from_bytes(&[9u8; 32]);
        let client_epk = [0xAAu8; 32];
        let accessory_epk = [0xBBu8; 32];

        let sub_tlv = verify_m3_sub_tlv(
            &client_ltsk,
            "client-pairing-id",
            &client_epk,
            &accessory_epk,
        );
        let items = tlv8::decode(&sub_tlv).unwrap();

        let id = tlv8::lookup(&items, tlv8::tags::IDENTIFIER).unwrap();
        assert_eq!(
            id, b"client-pairing-id",
            "M3 carries iOSDevicePairingID; sending AccessoryPairingID looks up a \
             pairing that cannot exist"
        );

        // The signature covers the same identifier, so substituting one later
        // would not help either.
        let sig: [u8; 64] = tlv8::lookup(&items, tlv8::tags::SIGNATURE)
            .unwrap()
            .try_into()
            .unwrap();
        let mut expected_info = Vec::new();
        expected_info.extend_from_slice(&client_epk);
        expected_info.extend_from_slice(b"client-pairing-id");
        expected_info.extend_from_slice(&accessory_epk);
        client_ltsk
            .verifying_key()
            .verify(&expected_info, &ed25519_dalek::Signature::from_bytes(&sig))
            .expect("signature must cover iOSDeviceEPK || iOSDevicePairingID || AccessoryEPK");
    }

    #[test]
    fn back_off_is_reported_as_a_delay_not_an_auth_failure() {
        let items = vec![
            tlv8::item_u8(tlv8::tags::STATE, 4),
            tlv8::item_u8(tlv8::tags::ERROR, tlv8::errors::BACKOFF),
            tlv8::item(tlv8::tags::RETRY_DELAY, vec![30]),
        ];
        let err = check_error(&items).expect_err("back-off must be an error");

        let back_off = err
            .downcast_ref::<HapBackOff>()
            .expect("callers must be able to tell a back-off from a bad PIN");
        assert_eq!(
            back_off.retry_after,
            Some(std::time::Duration::from_secs(30))
        );

        let msg = err.to_string();
        assert!(msg.contains("30"), "the delay must be named: {msg}");
        assert!(
            !msg.to_lowercase().contains("authentication failed"),
            "must not read as a credential problem: {msg}"
        );
    }

    #[test]
    fn retry_delay_is_a_little_endian_integer_of_any_width() {
        let ms = |bytes: Vec<u8>| {
            retry_delay_secs(&[tlv8::item(tlv8::tags::RETRY_DELAY, bytes)]).unwrap()
        };
        assert_eq!(ms(vec![30]), 30);
        assert_eq!(ms(vec![0x2C, 0x01]), 300);
        assert_eq!(ms(vec![0x10, 0x0E, 0x00, 0x00]), 3600);

        // Absent, empty, and over-wide values are "no delay stated", not zero.
        assert_eq!(retry_delay_secs(&[]), None);
        assert_eq!(
            retry_delay_secs(&[tlv8::item(tlv8::tags::RETRY_DELAY, vec![])]),
            None
        );
    }

    #[test]
    fn back_off_without_a_delay_still_says_what_happened() {
        let items = vec![tlv8::item_u8(tlv8::tags::ERROR, tlv8::errors::BACKOFF)];
        let err = check_error(&items).expect_err("back-off must be an error");
        assert_eq!(
            err.downcast_ref::<HapBackOff>().unwrap().retry_after,
            None,
            "no RetryDelay TLV means no delay is known — not a delay of zero"
        );
        assert!(err.to_string().contains("rate-limiting"));
    }

    #[test]
    fn an_empty_or_none_error_tlv_is_success() {
        // Zero-length ERROR TLV: no code was sent, so nothing failed. This used
        // to be reported as "HAP error 0: Unknown HAP error".
        assert!(check_error(&[tlv8::item(tlv8::tags::ERROR, vec![])]).is_ok());
        // kTLVError_None is an explicit success.
        assert!(check_error(&[tlv8::item_u8(tlv8::tags::ERROR, tlv8::errors::NONE)]).is_ok());
        // No ERROR TLV at all.
        assert!(check_error(&[tlv8::item_u8(tlv8::tags::STATE, 2)]).is_ok());
    }

    #[test]
    fn real_error_codes_are_still_errors() {
        let err = check_error(&[tlv8::item_u8(
            tlv8::tags::ERROR,
            tlv8::errors::AUTHENTICATION,
        )])
        .expect_err("0x02 is a real failure");
        assert!(err.to_string().contains("Authentication failed"));
        assert!(
            err.downcast_ref::<HapBackOff>().is_none(),
            "an auth failure is not a back-off"
        );

        assert!(check_error(&[tlv8::item_u8(tlv8::tags::ERROR, 0x7F)]).is_err());
    }

    /// Reference: pyatv 0.18.0 `pyatv/auth/hap_tlv8.py` —
    /// `class Flags(IntEnum): TransientPairing = 0x10`.
    ///
    /// The previous value, `0x02`, is not a defined pairing flag in any
    /// reference implementation.
    #[test]
    fn transient_flag_matches_pyatv() {
        assert_eq!(FLAG_TRANSIENT, 0x10);

        let m1 = tlv8::encode(&[
            tlv8::item_u8(tlv8::tags::STATE, 1),
            tlv8::item_u8(tlv8::tags::METHOD, tlv8::methods::PAIR_SETUP),
            tlv8::item_u8(tlv8::tags::FLAGS, FLAG_TRANSIENT),
        ]);
        // Single byte, as pyatv writes it — tag 0x13, length 1, value 0x10.
        assert_eq!(&m1[m1.len() - 3..], &[0x13, 0x01, 0x10]);
    }
}
