# Design: AirPlay receiver on Linux

**Status: design only. No code written.** This document exists so the FairPlay
decision can be made before anyone starts building.

The goal is the direction OpenPlay does not currently support: an iPhone, iPad
or Mac casts **to** a Linux machine, which decodes and displays the stream. That
is what UxPlay and RPiPlay do. Everything OpenPlay has today points the other
way — Linux sending to an Apple device.

## The gate, stated first

**An AirPlay receiver cannot work without FairPlay.** iOS and macOS senders
issue `POST /fp-setup` before they will stream, and they expect a response
derived from Apple's fixed key tables. There is no negotiation path that skips
it and no fallback a receiver can offer.

This is the same blocker as issue #8, arriving from the other side. On the
sender side it only affects receivers that demand FairPlay, so some casting
works without it. On the receiver side it affects **every** Apple sender, so
nothing works without it.

Concretely: items 1–5 below can all be built and tested, and the result will
still refuse every real iPhone until the FairPlay decision is made.

That decision — whether to port Apple's key material from the GPL projects that
carry it — is recorded as unmade in [crypto.md](crypto.md#fairplay--not-fixed).

## Evidence from a real receiver

Probing a MacBook Air (`Mac16,12`, AirTunes/950.7.1) with
`cargo run -p openplay-airplay --example pair_probe` produced the shape a Linux
receiver would have to imitate:

```
GET /info  → 200, 1157-byte binary plist
             features 0x38174FDE4A7FCFD5
             mirroring true, video true, audio true
             HK pairing required true, transient pairing true
```

Every other endpoint answered `403` because that Mac's AirPlay Receiver setting
does not admit this device. Worth noting for testing: **access policy is
enforced before pairing**, so a receiver implementation needs its own notion of
who may connect, and must answer 403 rather than failing later.

## Components

Ordered by dependency. Effort is rough, for one developer.

### 1. mDNS advertising — small

Advertise two services on port 7000:

- `_airplay._tcp.local.` — TXT keys `deviceid`, `features`, `model`, `srcvers`,
  `pk` (our Ed25519 public key), `flags`, `protovers`
- `_raop._tcp.local.` — TXT keys `txtvers`, `ch`, `cn`, `et`, `sr`, `ss`, `tp`,
  `vs`, `am`, `pk`

`openplay-discovery` already advertises via `mdns-sd`, but
`ReceiverAdvertiser::new` is hardcoded to `SERVICE_TYPE`
(`_openplay._tcp.local.`) and takes a `TxtRecord` shaped for OpenPlay. It needs
generalising to an arbitrary service type and TXT map.

The `features` value must be *emitted*, not just parsed. `features.rs` currently
only parses; the bit definitions are there and can be reused.

### 2. HTTP/RTSP server on port 7000 — medium

AirPlay multiplexes plain HTTP and RTSP-over-HTTP on one port. Needs:

| Endpoint | Purpose |
|---|---|
| `GET /info` | Binary plist of our capabilities |
| `POST /pair-setup`, `/pair-verify` | HAP, server side |
| `POST /fp-setup` | FairPlay, server side — **blocked** |
| `RTSP ANNOUNCE / SETUP / RECORD / PLAY / TEARDOWN` | Audio session |
| `POST /stream` | Mirroring session |
| `POST /feedback`, `GET /playback-info` | Keepalive and state |

`http_session.rs` has request/response plumbing and plist parsing, but it is
written as a *client*. The plist and header handling is reusable; the direction
is not.

Note the lesson already learned here: **check status codes and answer with
correct ones**. The client side reported a 403 as "Missing state TLV" until it
was fixed; a receiver that returns misleading statuses will waste the same time
in the other direction.

### 3. HAP pairing, server side — medium, mostly done

This is the pleasant surprise. The SRP-6a *server* side already exists in this
repository as a working implementation: `srp::tests::RefServer`, written to
cross-check the client. It computes `x`, the verifier `v = g^x`, `B = k*v + g^b`,
the shared `S = (A * v^u)^b`, `K`, `M1` and `M2` — which is the whole server-side
SRP computation.

Promoting it from a test harness to `srp::server_compute` is a small, well-tested
step, and the RFC 5054 group and its property tests are already in place.

What remains is the surrounding HAP flow inverted:

- M1→M2: generate salt, return `s` and `B`
- M3→M4: verify the client's `M1`, return `M2`
- M5→M6: decrypt the client's sub-TLV with ChaCha20-Poly1305, verify its Ed25519
  signature, persist the pairing, return our own signed accessory info
- `pair-verify`: X25519 ECDH, both directions

`hap_pairing.rs` has every primitive — HKDF salts and info strings, the nonces
(`PS-Msg05`, `PS-Msg06`), TLV8, Ed25519 — used in the client direction.
`tlv8.rs` is symmetric and needs no changes.

### 4. FairPlay, server side — **blocked**

Respond to the sender's `fp-setup` rounds and derive the AES key protecting the
media streams.

`fairplay.rs` has the message framing, which is direction-agnostic. It does not
have the key tables or the challenge-response transform, and cannot until the
decision in [crypto.md](crypto.md#fairplay--not-fixed) is made.

**Do not attempt this by inventing constants.** That is precisely how the
current placeholder came to exist and how issue #8 was filed.

### 5. Media receive and render — large

**Mirroring.** H.264 over the `/stream` connection, framed with the AirPlay
mirror header and encrypted with the key from FairPlay. `mirror_header.rs`
already has `MirrorHeader::decode` and a `PacketType` enum, so parsing incoming
frames is largely covered. Decode and display then needs a GStreamer pipeline —
`ReceiverPipeline` in `openplay-pipeline` exists but is built around
`webrtcbin`, so it is a template rather than a drop-in.

**Audio (RAOP).** ALAC or AAC-ELD over RTP with AES-128-CBC, plus retransmission
and a jitter buffer. This is genuinely separate work from video and is the part
most often underestimated.

**Timing.** `ntp.rs` implements the timing channel client-side; the receiver
needs to answer timing requests instead.

### 6. Display and UI — medium

`openplay-receiver` currently shows a static waiting page. It would need to host
the session, render decoded video, and handle connect/disconnect. Note this
overlaps with wiring up the OpenPlay/WebRTC receiver path, which is also
unfinished — doing both at once is probably cheaper than doing them separately.

## Suggested order

1. Promote `RefServer` to `srp::server_compute`, with tests against the existing
   client — pure win, no blockers, useful regardless
2. Generalise `ReceiverAdvertiser` to arbitrary service types and TXT records
3. HTTP server skeleton with `/info` answering a real plist
4. HAP pair-setup and pair-verify, server side — at this point an Apple device
   will pair and then stop at FairPlay
5. **Decision point on FairPlay**
6. Mirroring receive and render
7. RAOP audio

Steps 1–4 are worth doing whatever is decided at step 5: they are testable
against the existing client implementation without any Apple hardware, and step
1 in particular strengthens what is already shipped.

## What this is not

**CarPlay is a different protocol.** It is not AirPlay with a different name: it
runs over USB via iAP2 (or wirelessly over a Bluetooth-initiated Wi-Fi link),
uses Apple's authentication coprocessor, and shares essentially no code with
`openplay-airplay`. If CarPlay support is wanted, it belongs in a separate
project rather than this crate.

## Honest summary

Steps 1–3 are a few days. Step 4 is perhaps a week and is the most
specification-heavy part, though the SRP core is already written and tested.
Steps 6–7 are the bulk of the work and where UxPlay's maturity shows.

None of it produces something an iPhone will cast to until FairPlay is resolved.
That is the decision to make first, and it is a licensing and policy judgement
rather than a technical one.
