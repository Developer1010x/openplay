# openplay-crypto

Self-signed ECDSA P-256 certificate lifecycle for the OpenPlay WebRTC connection:
generate, persist, load and fingerprint.

> **Status: never constructed.** `CertificateManager` has no callers outside this
> crate's own tests, so no certificate is generated on first launch or at any
> other time. The WebRTC path that would consume it is unwired. See
> [docs/crypto.md](../../docs/crypto.md#tls-certificates).

## What it contains

**`CertificateManager`** — the main type. Wraps a certificate and private key in PEM, DER, and fingerprint form.

- `generate()` — creates a fresh self-signed cert valid 2024–2034 with CN `OpenPlay Device`.
- `load_or_generate(data_dir)` — loads `openplay.crt.pem` + `openplay.key.pem` from `data_dir` if they exist, otherwise generates and saves them. The private key is written with mode `0o600` on Unix.
- Accessors: `cert_pem()`, `key_pem()`, `cert_der()`, `fingerprint()`.
- Static helpers: `cert_path(data_dir)`, `key_path(data_dir)`.

**`certificate_fingerprint(der_bytes)`** — computes a SHA-256 fingerprint of DER-encoded cert bytes and returns it as a colon-separated uppercase hex string (e.g. `AA:BB:CC:...`). This fingerprint is exchanged during signaling so both sides can verify device identity.

## Why a dedicated crate

The certificate is *intended* to be shared between the signaling server
(receiver), the signaling client (sender), and the WebRTC DTLS layer. Isolating it
avoids circular dependencies between `openplay-signaling` and `openplay-receiver`.

Note this crate does not depend on `rustls` — it only produces the certificate and
key; whoever wires up signaling is responsible for building the rustls config from
them.

## Tests

```bash
cargo test -p openplay-crypto
```
