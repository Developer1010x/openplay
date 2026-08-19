# Cryptography status

This document exists because two of the crypto modules in this repository once
looked complete, were well commented, and could never have worked. Anyone
debugging an AirPlay receiver that "just rejects the connection" should read
this before reaching for tcpdump.

Original report: issue #8 (closed). Hardware confirmation is tracked in issue #27.

## Summary

| Component | Where | Status |
|---|---|---|
| HAP pair-setup (SRP-6a) | `airplay/srp.rs`, `hap_pairing.rs` | **Fixed**, untested against hardware |
| HAP pair-verify | `airplay/hap_pairing.rs` | Implemented, unreachable until pairing is confirmed |
| FairPlay | `airplay/fairplay.rs` | **Will not be implemented** (decision below), and not wired in — `fp_setup` has no callers |
| TLS certificates | `openplay-crypto/certs.rs` | Implemented, never constructed anywhere |

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

### Caveat: still unconfirmed against hardware

**This has not been confirmed against physical Apple hardware.** The known
blocker is removed; that is not the same as proven working. No test in this
repository can substitute for a real receiver.

There is a probe for exactly this:

```console
cargo run -p openplay-airplay --example pair_probe -- <ip>:7000        # transient
cargo run -p openplay-airplay --example pair_probe -- <ip>:7000 1234   # with PIN
```

### What a real attempt actually produced

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

So the SRP question is still open. The attempt was not wasted, though: a 403 was
being reported as `Missing state TLV`, because `recv_response` never looked at
the HTTP status line and an empty body failed TLV8 decoding. That is precisely
the misleading-diagnostic problem this issue was filed about, one layer up. It
now reports the status and names the setting to change (`check_http_status` in
`hap_pairing.rs`, with four tests).

If you get further against real hardware, please add the result to issue #27
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
