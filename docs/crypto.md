# Cryptography status

This document exists because two of the crypto modules in this repository once
looked complete, were well commented, and could never have worked. Anyone
debugging an AirPlay receiver that "just rejects the connection" should read
this before reaching for tcpdump.

Original report: issue #8 (closed). Hardware confirmation is tracked in issue #27.

## Summary

| Component | Where | Status |
|---|---|---|
| HAP transient pair-setup (SRP-6a) | `airplay/srp.rs`, `hap_pairing.rs` | **Fixed and confirmed against hardware** — M1–M4 against AirTunes/950.7.1 |
| Encrypted control channel | `airplay/control_channel.rs` | Implemented and confirmed against hardware — an encrypted `GET /info` returns 200. The mirror stream cannot write into it yet, so mirroring is unconfirmed |
| HAP pair-verify | `airplay/hap_pairing.rs` | Implemented, no callers — the transient flow ends at M4 and keys the control channel directly; pair-verify belongs to the PIN flow, which the session path never uses |
| FairPlay | `airplay/fairplay.rs` | **Will not be implemented** (decision below), and not wired in — `fp_setup` has no callers |
| TLS certificates | `openplay-crypto/certs.rs` | Implemented, never constructed anywhere |
| Signaling TLS config | `openplay-crypto/tls.rs` | Implemented, no callers — fingerprint pinning, which is **not** peer authentication |

## HAP pair-setup — fixed

### What was wrong

`SRP_N_HEX` claimed in its doc comment to be "SRP-6a 3072-bit prime (RFC 5054
appendix A)". It began with the right digits and then diverged. Three
independently checkable facts:

- 772 hex digits — **3088 bits, not 3072**
- **not equal** to the RFC 5054 appendix A prime
- **composite** (Miller-Rabin, bases 2..37)

SRP requires both parties to agree on `N`, and requires `N` to be prime. Neither
held, so no `pair_setup` could ever agree on a session key with a real
accessory. The failure appears at the SRP proof exchange (M3/M4), which looks
like a rejection rather than a crypto fault — hence the wasted debugging time.

### What it is now

The modulus is the real RFC 3526 group 15, which RFC 5054 appendix A adopts for
SRP with `g = 5`. It is **generated, not copied**:

```
N = 2^3072 - 2^3008 - 1 + 2^64 * ( floor(2^2942 * pi) + 1690314 )
```

`srp::tests::srp_group_matches_rfc3526_formula` checks `N` against a rearrangement
of that formula rather than recomputing pi to 900 digits, bounding the pi term to
about 21 decimal digits. Be precise about what that does and does not pin: it
constrains roughly the top 130 bits and the low 64 bits of `N`. A corrupted digit
in the middle would pass *this* test — it is caught by the primality and
safe-primality tests instead, which is why all of them exist rather than any one
of them.

The other constant tests check the properties SRP actually depends on: 3072 bits,
prime, `(N-1)/2` prime, and `g = 5` a generator of the full group.

On that last point: RFC 3526 itself specifies `g = 2`, but SRP needs a generator
of the whole of `Z_N*`, and 2 is a quadratic residue mod this N while 5 is not.
The test asserts both — `5^q == N-1` and `2^q == 1`.

### Why the tests are shaped the way they are

`client_and_reference_server_agree` runs the client against an independently
written implementation of the SRP server side. That is the test that proves
interoperability of the *math*.

**It is not sufficient on its own, and this matters.** When the old fabricated
constant is restored, 6 of the 9 tests fail — but the round-trip tests still
pass, because client and reference server share the same `N` and will agree on a
wrong group just as happily as a right one.

That is the same self-consistency trap the original issue identified in the
FairPlay tests, which pass by checking that the seed decodes to its own ASCII
string and that the cipher round-trips with the key it was handed. A round-trip
test alone would have let this bug ship twice.

So the constant's *properties* are pinned separately from the protocol
round-trip. If you touch `SRP_N_HEX`, expect six tests to shout.

### Other changes made at the same time

The rest of the SRP-6a math was audited against RFC 5054 and was already
correct: `u`, `x`, `k`, `S`, `K`, `M1` and `M2` all match, with SHA-512 as HAP
requires. Two aborts RFC 5054 §2.5.3 mandates were missing and have been added —
reject `B mod N == 0` and `u == 0`. Both indicate a broken or hostile server,
and continuing would derive a session key an attacker can predict.

The private exponent was reduced from 2048 bits to 256. The old code called
`random_bigint(256)` against a parameter named `bytes`.

### Confirmed against hardware

Transient pair-setup **has been confirmed against physical Apple hardware**
([#27](https://github.com/Developer1010x/openplay/issues/27), 2026-08-26): a
Mac running AirTunes/950.7.1 with AirPlay Receiver set to *Everyone* runs the
SRP exchange to completion and the probe reports
`SRP-6a verification successful`.

Two facts from that run are worth more than the bare result:

- **The receiver's public key is 384 bytes** — 3072 bits. A receiver on a
  different modulus would not produce a `B` of that width, so the group is now
  corroborated from the far side of the wire, not only by the self-checks in
  `srp.rs`.
- **It did not work as shipped.** Reaching M4 took two fixes, both real bugs
  rather than environment problems. `POST /pair-setup` needs an
  `X-Apple-HKP: 4` header, or the receiver answers 400 before reading the body
  (#41). And the M1 proof must hash `g`'s minimal encoding — the single byte
  `0x05` — not `PAD(g)`; with padding the receiver answers HAP error `2` at M4
  (#42, recovered in #43). The neighbouring `k = H(N | PAD(g))` genuinely does
  need padding, which is how the bug got written.

Transient pairing **ends at M4**. There is no M5/M6, no long-term keys and no
pair-verify. Running M5 anyway makes the receiver close the connection right
after an otherwise successful M4 — which reads exactly like a crypto failure
and is not one. From M4 on the connection is encrypted: HKDF-SHA512 under
`Control-Salt` with the `Control-Write-Encryption-Key` /
`Control-Read-Encryption-Key` info strings, then ChaCha20-Poly1305 frames with
a 2-byte little-endian length as AEAD associated data and a per-direction
64-bit counter nonce. `control_channel.rs` implements that, and an encrypted
`GET /info` over it returns `HTTP/1.1 200 OK`.

**A trap when re-testing.** The receiver backs off hard after a failed
pair-setup, answering HAP error `0x03` with a retry delay. Attempts closer
together than roughly two minutes return backoff rather than a real answer, and
backoff looks nothing like an authentication failure. Treat any `0x03` as "no
result", wait, and re-run.

**What this does not establish: mirroring.** `MirrorStream` writes NAL units
to a raw `TcpStream`, but every byte after M4 must be wrapped in
control-channel frames, so `negotiate_with_auth` deliberately stops after
`POST /stream` with an explicit error rather than emit plaintext into an
encrypted connection. Making the mirror stream encryption-aware is the
remaining work. Confirming it end to end also needs a receiver that mirrors
without FairPlay: a modern Mac gates mirroring behind it (`/fp-setup` answers
400, RTSP `SETUP` 455, `/stream` 404), and FairPlay stays out of scope by the
decision below. A software receiver such as `uxplay` is probably the cheapest
way to get one.

The probes that produced all of this:

```console
cargo run -p openplay-airplay --example pair_probe -- <ip>:7000        # transient, stops after M4
cargo run -p openplay-airplay --example pair_probe -- <ip>:7000 1234   # with PIN
cargo run -p openplay-airplay --example control_probe -- <ip>:7000     # M4, then the encrypted channel
```

### What the first attempt produced

Run against a MacBook Air (`Mac16,12`, AirTunes/950.7.1), `GET /info` succeeded —
1157-byte plist, features `0x38174FDE4A7FCFD5`, mirroring, video and audio all
advertised, HK pairing required, transient pairing supported.

**Every other endpoint answered `403 Forbidden`** with an empty body:
`/pair-setup`, `/pair-pin-start`, `/fp-setup`, `/server-info`, `/auth-setup`.
Adding `User-Agent` and `X-Apple-*` headers changed nothing.

That is macOS's AirPlay Receiver *access policy*, not a pairing failure. Its
default setting is "Current User", which refuses any device not signed into the
same Apple ID, and it is enforced before any crypto runs. To test the SRP path
the receiver must be set to "Anyone on the same network" in
**System Settings → General → AirDrop & Handoff → AirPlay Receiver**.

That attempt was not wasted: a 403 was being reported as `Missing state TLV`,
because `recv_response` never looked at the HTTP status line and an empty body
failed TLV8 decoding. That is precisely the misleading-diagnostic problem this
issue was filed about, one layer up. It now reports the status and names the
setting to change (`check_http_status` in `hap_pairing.rs`, with four tests).

With the setting changed, the second attempt is the one described above. If you
get a different result against other hardware, please add it to issue #27
either way.

## FairPlay — not fixed

Apple TV 3rd gen and some other receivers require FairPlay authentication before
accepting a mirror stream. `fairplay.rs` implements the three-round
`POST /fp-setup` framing correctly, and then derives the AES-128 key as:

```
key = SHA-512(server_data || FAIRPLAY_SEED)
```

where `FAIRPLAY_SEED` is the ASCII string `AirPlay-FairPlay-Setup-Key-Seed1`.
Real FairPlay uses Apple's fixed key tables and a specific challenge-response
transform. Neither is present, so every key derived here would be wrong.

**The module is not wired in.** `fp_setup` has no callers: `session.rs` never
references `fairplay.rs`. Instead `negotiate_with_auth` reads the model string
from `/info` and refuses `AppleTV2,*` / `AppleTV3,*` up front with an explicit
"requires FairPlay authentication which is not supported" error. That is a better
failure than a mysterious reset, and it means the placeholder keys are never
actually put on the wire.

`fp_setup` does log a warning on entry (`fairplay.rs:94`) saying it cannot
interoperate — but since nothing calls it, that warning never fires. Treat the
module as documented dead code; the decision below is why it stays that way.

### Decision: FairPlay will not be implemented here

**This is settled, not pending.** Issue #8 raised porting the real key material
as future work; the answer is no, and this section is the record of that so the
question does not get reopened as an oversight.

What a port would require is two separate things, which is why it was never
comparable to the SRP fix:

1. The key tables
2. The challenge-response transform

RPiPlay and UxPlay both implement this and are GPL, so their *code* is
license-compatible with this project. But the thing that would actually have to
be copied is not really code — it is Apple's fixed key tables, and the transform
exists to enforce FairPlay DRM. Vendoring it here is DRM circumvention
regardless of which repository it is copied from, and GPL compatibility does not
change that.

Note the asymmetry with SRP, which is why one was fixed and the other will not
be. SRP was a single published IETF constant: derivable from a formula, and
verifiable from first principles by anyone, which is exactly what
`srp.rs`'s property tests now do. FairPlay is proprietary key material that
cannot be derived, only copied, and cannot be verified without hardware. A fix
we could not verify would be indistinguishable from the bug we just removed.

**Consequences, stated plainly:**

- Apple TV 2nd and 3rd generation will not be supported. They are refused up
  front by model string, which is the honest failure.
- Receivers that do not demand FairPlay — modern Apple TVs, AirPlay 2 TVs — are
  unaffected. They were never blocked by this.
- `fairplay.rs` stays in the tree as documented dead code rather than being
  deleted, because the `POST /fp-setup` framing is correct and useful as
  protocol documentation. It has no callers and must not acquire any.

Anyone who disagrees is free to fork — that is what the GPL is for. It will not
land in this repository.

### Do not "fix" this by guessing

The original bug was invented constants that looked plausible and passed their
own tests. Replacing them with different invented constants would reproduce the
defect exactly. If you cannot verify the values against a working
implementation, leave them and leave the warning in place.

## TLS certificates

`openplay-crypto` implements a full self-signed ECDSA P-256 certificate
lifecycle — `CertificateManager::load_or_generate`, `generate`, `cert_pem`,
`key_pem`, `cert_der`, `fingerprint`, and path helpers.

It is **never constructed outside its own tests**. The README used to claim
certificates were "generated on first launch"; they are not, because nothing
calls this crate. The signaling layer takes an `Arc<ClientConfig>` /
`Arc<ServerConfig>` from the caller, and no caller exists yet.

This is not a defect in the crypto — it is part of the OpenPlay/WebRTC path not
being wired up. See [protocols.md](protocols.md#openplay-webrtc).
