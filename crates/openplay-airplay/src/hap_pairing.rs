//! HomeKit Accessory Protocol (HAP) pairing for AirPlay.
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
use num_bigint::BigUint;
use sha2::{Digest, Sha512};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey};

use crate::tlv8;

/// SRP-6a 3072-bit prime (RFC 5054 appendix A).
const SRP_N_HEX: &str = "\
FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74\
020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437\
4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05\
98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9\
ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE3\
9E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718399\
5AC44B1B0A8E5CF4B2A6BDEE14C7B62C8B8628018B68D3F2CEE6E0868B1A3A15\
5E2ADED1F175BE4F6C3454D4412C08A235C079F4CC02F3A6311A49C20B874B3A4\
14E4D5F5A1E10AC32F7B34A1BDEF5E307AFDC2B2C19C75CE2F330E7B4F0872BC\
BC8D32F5BBE9A6B52D91A6C2157CECD13EB4AD5B14CF46F0DF9543C5FBD8B38B\
E884CA77EE55D2C7116CDC7D3FC19BE6D7E5E6949BD66A2B0D8DC1E2A3D0C9B3";

const SRP_G: u32 = 5;

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
    /// Accessory's Ed25519 public key.
    pub accessory_ltpk: [u8; 32],
    /// Accessory device identifier.
    pub accessory_id: String,
}

/// Result of a successful pair-verify: the shared encryption key for the session.
#[derive(Debug, Clone)]
pub struct PairVerifyResult {
    /// Shared session encryption key (derived from ECDH).
    pub shared_key: [u8; 32],
}

/// Stored pairing information for a device.
#[derive(Debug, Clone)]
pub struct PairedDevice {
    pub device_id: String,
    pub accessory_ltpk: [u8; 32],
    pub client_ltsk: [u8; 32],
    pub client_ltpk: [u8; 32],
}

/// Perform transient pair-setup with an AirPlay 2 receiver (no PIN required).
///
/// Transient pairing is used when the receiver allows "Everyone on the Same Network"
/// access. The flags=0x02 (Transient) tells the receiver that no PIN dialog is needed.
/// Uses SRP-6a with PIN "3939" (standard AirPlay transient code).
///
/// Returns a PairSetupResult that can be used for pair-verify.
pub async fn pair_setup_transient(addr: SocketAddr) -> anyhow::Result<PairSetupResult> {
    pair_setup_internal(addr, "3939", true).await
}

/// Perform pair-setup with an AirPlay receiver using a 4-digit PIN.
///
/// This is the first-time pairing flow using SRP-6a.
pub async fn pair_setup(addr: SocketAddr, pin: &str) -> anyhow::Result<PairSetupResult> {
    pair_setup_internal(addr, pin, false).await
}

async fn pair_setup_internal(
    addr: SocketAddr,
    pin: &str,
    transient: bool,
) -> anyhow::Result<PairSetupResult> {
    let mut stream = TcpStream::connect(addr).await?;
    info!(%addr, transient, "Starting HAP pair-setup");

    // M1: Client → Server: State=1, Method=PairSetup, [Flags=Transient]
    let m1 = if transient {
        tlv8::encode(&[
            tlv8::item_u8(tlv8::tags::STATE, 1),
            tlv8::item_u8(tlv8::tags::METHOD, tlv8::methods::PAIR_SETUP),
            tlv8::item_u8(tlv8::tags::FLAGS, 0x02), // Transient
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

    // SRP-6a client computation
    let n = BigUint::parse_bytes(SRP_N_HEX.as_bytes(), 16)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse SRP prime"))?;
    let g = BigUint::from(SRP_G);

    // Generate private key a, compute A = g^a mod N
    let a = random_bigint(256);
    let big_a = g.modpow(&a, &n);
    let big_a_bytes = pad_to_n(&big_a, &n);

    let big_b = BigUint::from_bytes_be(server_pk_bytes);

    // u = SHA-512(A | B)
    let u = {
        let mut hasher = Sha512::new();
        hasher.update(&big_a_bytes);
        hasher.update(&pad_to_n(&big_b, &n));
        BigUint::from_bytes_be(&hasher.finalize())
    };

    // x = SHA-512(salt | SHA-512(username | ":" | password))
    let x = {
        let inner = {
            let mut h = Sha512::new();
            h.update(PAIR_SETUP_USERNAME.as_bytes());
            h.update(b":");
            h.update(pin.as_bytes());
            h.finalize()
        };
        let mut h = Sha512::new();
        h.update(salt);
        h.update(&inner);
        BigUint::from_bytes_be(&h.finalize())
    };

    // k = SHA-512(N | pad(g))
    let k = {
        let mut h = Sha512::new();
        h.update(&n.to_bytes_be());
        h.update(&pad_to_n(&g, &n));
        BigUint::from_bytes_be(&h.finalize())
    };

    // S = (B - k * g^x mod N) ^ (a + u * x) mod N
    let gx = g.modpow(&x, &n);
    let kgx = (&k * &gx) % &n;
    let diff = if big_b >= kgx {
        &big_b - &kgx
    } else {
        &n - &kgx + &big_b
    };
    let exp = &a + &u * &x;
    let big_s = diff.modpow(&exp, &n);

    // K = SHA-512(S)
    let session_key = {
        let mut h = Sha512::new();
        h.update(&pad_to_n(&big_s, &n));
        h.finalize()
    };

    // M1 proof = SHA-512(SHA-512(N) XOR SHA-512(g) | SHA-512(username) | salt | A | B | K)
    let proof_m1 = {
        let hash_n = Sha512::digest(&n.to_bytes_be());
        let hash_g = Sha512::digest(&pad_to_n(&g, &n));
        let hash_xor: Vec<u8> = hash_n
            .iter()
            .zip(hash_g.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let hash_user = Sha512::digest(PAIR_SETUP_USERNAME.as_bytes());

        let mut h = Sha512::new();
        h.update(&hash_xor);
        h.update(&hash_user);
        h.update(salt);
        h.update(&big_a_bytes);
        h.update(&pad_to_n(&big_b, &n));
        h.update(&session_key);
        h.finalize().to_vec()
    };

    // M3: Client → Server: State=3, PublicKey=A, Proof=M1
    let m3 = tlv8::encode(&[
        tlv8::item_u8(tlv8::tags::STATE, 3),
        tlv8::item(tlv8::tags::PUBLIC_KEY, big_a_bytes.clone()),
        tlv8::item(tlv8::tags::PROOF, proof_m1.clone()),
    ]);
    send_pair_setup(&mut stream, &m3).await?;

    // M4: Server → Client: State=4, Proof=M2
    let m4_data = recv_pair_setup(&mut stream).await?;
    let m4 = tlv8::decode(&m4_data)?;
    check_error(&m4)?;
    check_state(&m4, 4)?;

    let server_proof = tlv8::lookup(&m4, tlv8::tags::PROOF)
        .ok_or_else(|| anyhow::anyhow!("M4: missing server proof"))?;

    // Verify server proof: M2 = SHA-512(A | M1 | K)
    let expected_m2 = {
        let mut h = Sha512::new();
        h.update(&big_a_bytes);
        h.update(&proof_m1);
        h.update(&session_key);
        h.finalize()
    };
    if server_proof != expected_m2.as_slice() {
        return Err(anyhow::anyhow!("Server proof verification failed"));
    }
    info!("SRP-6a verification successful");

    // Derive encryption key for M5/M6 exchange
    let enc_key = hkdf_derive(
        b"Pair-Setup-Encrypt-Salt",
        &session_key,
        b"Pair-Setup-Encrypt-Info",
        32,
    )?;

    // Generate our long-term Ed25519 key pair
    let client_ltsk = SigningKey::generate(&mut rand::thread_rng());
    let client_ltpk = client_ltsk.verifying_key();

    // Derive iOSDeviceX
    let device_x = hkdf_derive(
        b"Pair-Setup-Controller-Sign-Salt",
        &session_key,
        b"Pair-Setup-Controller-Sign-Info",
        32,
    )?;

    // iOSDeviceInfo = iOSDeviceX || iOSDevicePairingID || iOSDeviceLTPK
    let device_id = uuid::Uuid::new_v4().to_string();
    let mut device_info = Vec::new();
    device_info.extend_from_slice(&device_x);
    device_info.extend_from_slice(device_id.as_bytes());
    device_info.extend_from_slice(client_ltpk.as_bytes());

    let device_sig = client_ltsk.sign(&device_info);

    // Encrypt sub-TLV with ChaCha20-Poly1305
    let sub_tlv = tlv8::encode(&[
        tlv8::item(tlv8::tags::IDENTIFIER, device_id.as_bytes().to_vec()),
        tlv8::item(tlv8::tags::PUBLIC_KEY, client_ltpk.as_bytes().to_vec()),
        tlv8::item(tlv8::tags::SIGNATURE, device_sig.to_bytes().to_vec()),
    ]);

    let enc_key_arr: [u8; 32] = enc_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("key length"))?;
    let cipher = ChaCha20Poly1305::new(&enc_key_arr.into());
    let nonce = Nonce::from_slice(b"PS-Msg05\x00\x00\x00\x00");
    let encrypted = cipher
        .encrypt(nonce, sub_tlv.as_ref())
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

    let nonce6 = Nonce::from_slice(b"PS-Msg06\x00\x00\x00\x00");
    let m6_decrypted = cipher
        .decrypt(nonce6, m6_encrypted)
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
        &session_key,
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
    let nonce2 = Nonce::from_slice(b"PV-Msg02\x00\x00\x00\x00");
    let m2_plain = cipher
        .decrypt(nonce2, m2_encrypted)
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

    // Build client proof
    let client_ltsk = SigningKey::from_bytes(&paired.client_ltsk);

    let mut client_info = Vec::new();
    client_info.extend_from_slice(client_public.as_bytes());
    client_info.extend_from_slice(paired.device_id.as_bytes());
    client_info.extend_from_slice(server_epk_bytes);

    let client_sig = client_ltsk.sign(&client_info);

    let sub_tlv = tlv8::encode(&[
        tlv8::item(tlv8::tags::IDENTIFIER, paired.device_id.as_bytes().to_vec()),
        tlv8::item(tlv8::tags::SIGNATURE, client_sig.to_bytes().to_vec()),
    ]);

    let nonce3 = Nonce::from_slice(b"PV-Msg03\x00\x00\x00\x00");
    let encrypted = cipher
        .encrypt(nonce3, sub_tlv.as_ref())
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

// --- HTTP helpers for /pair-setup and /pair-verify ---

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
        "POST {} HTTP/1.1\r\nContent-Type: {}\r\nContent-Length: {}\r\n\r\n",
        path,
        content_type,
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

fn check_error(items: &[tlv8::Tlv8Item]) -> anyhow::Result<()> {
    if let Some(err) = tlv8::lookup(items, tlv8::tags::ERROR) {
        let code = err.first().copied().unwrap_or(0);
        let msg = match code {
            tlv8::errors::UNKNOWN => "Unknown error",
            tlv8::errors::AUTHENTICATION => "Authentication failed",
            tlv8::errors::BACKOFF => "Too many attempts, try again later",
            tlv8::errors::MAX_PEERS => "Maximum peers reached",
            tlv8::errors::MAX_TRIES => "Maximum tries reached",
            tlv8::errors::UNAVAILABLE => "Resource unavailable",
            tlv8::errors::BUSY => "Device busy",
            _ => "Unknown HAP error",
        };
        return Err(anyhow::anyhow!("HAP error {code}: {msg}"));
    }
    Ok(())
}

// --- Crypto helpers ---

fn hkdf_derive(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> anyhow::Result<Vec<u8>> {
    let hk = Hkdf::<Sha512>::new(Some(salt), ikm);
    let mut okm = vec![0u8; len];
    hk.expand(info, &mut okm)
        .map_err(|e| anyhow::anyhow!("HKDF expand failed: {e}"))?;
    Ok(okm)
}

fn random_bigint(bytes: usize) -> BigUint {
    let mut rng_bytes = vec![0u8; bytes];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut rng_bytes);
    BigUint::from_bytes_be(&rng_bytes)
}

fn pad_to_n(value: &BigUint, n: &BigUint) -> Vec<u8> {
    let n_len = n.to_bytes_be().len();
    let bytes = value.to_bytes_be();
    if bytes.len() >= n_len {
        bytes
    } else {
        let mut padded = vec![0u8; n_len - bytes.len()];
        padded.extend_from_slice(&bytes);
        padded
    }
}

// --- Paired device storage ---

/// Initialize the paired devices SQLite database.
pub fn init_paired_db(db_path: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS paired_devices (
            device_id TEXT PRIMARY KEY,
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
        "INSERT OR REPLACE INTO paired_devices (device_id, accessory_ltpk, client_ltsk, client_ltpk)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            device.device_id,
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
        "SELECT device_id, accessory_ltpk, client_ltsk, client_ltpk FROM paired_devices WHERE device_id = ?1"
    )?;

    let mut rows = stmt.query(rusqlite::params![device_id])?;
    if let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let ltpk: Vec<u8> = row.get(1)?;
        let ltsk: Vec<u8> = row.get(2)?;
        let ltpk_client: Vec<u8> = row.get(3)?;

        Ok(Some(PairedDevice {
            device_id: id,
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
    fn test_srp_n_parses() {
        let n = BigUint::parse_bytes(SRP_N_HEX.as_bytes(), 16);
        assert!(n.is_some());
        let n = n.unwrap();
        assert!(n > BigUint::from(1u32));
        // 3072-bit prime should be ~384 bytes (may include leading bytes)
        let n_bytes = n.to_bytes_be().len();
        assert!(
            n_bytes >= 384 && n_bytes <= 386,
            "N should be ~384 bytes, got {n_bytes}"
        );
    }

    #[test]
    fn test_pad_to_n() {
        let n = BigUint::from(256u32);
        let val = BigUint::from(1u32);
        let padded = pad_to_n(&val, &n);
        assert_eq!(padded.len(), 2); // N is 2 bytes
        assert_eq!(padded, &[0, 1]);
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

        let ids = list_paired_devices(&conn).unwrap();
        assert_eq!(ids, vec!["test-device-001"]);
    }
}
