# Troubleshooting

Start by raising the log level — most of these are diagnosable from `debug`:

```bash
RUST_LOG=debug ./openplay-sender
```

## Before anything else: is it a known gap?

Several symptoms that look like bugs are unimplemented features. Check here
first.

| Symptom | Cause |
|---|---|
| An OpenPlay cast fails immediately, with nothing useful in the log | Almost always the missing `nice` GStreamer plugin — see [below](#openplay-webrtc) |
| Sender says "Casting..." and nothing happens for a long time | Expected. It is waiting for a human at the receiver to press **Allow**, with no timeout |
| Sender is told "The person at the receiver declined the cast" | Somebody pressed **Deny** |
| Sender is told the receiver "did not publish a certificate fingerprint" | The receiver's mDNS TXT record had no `fp` key. The sender refuses to connect unpinned, by design |
| Video casts but there is no sound | There is no audio support at all, on any protocol |
| A Miracast cast ends within moments of the handshake succeeding | Was a defect in the session lifecycle, fixed; if you still see it, that is a bug worth reporting — see [Miracast](#miracast) |
| No `config.toml` appeared before commit c06e1f7 | Fixed — `AppConfig::load_or_create_at` writes the defaults on first launch |
| AirPlay refuses an older Apple TV up front | `AppleTV2,*`/`AppleTV3,*` are rejected by model because FairPlay is unimplemented, see [crypto.md](crypto.md) |
| No screen capture on macOS | Capture there relies on GStreamer's `screencapturesrc`/`avfvideosrc` and has never been verified |
| Windows build used to fail outright | `openplay-capture` used the `windows` crate without declaring it; fixed, but still untested |

## Screen capture

### The portal dialog never appears (Linux)

OpenPlay requests capture through the XDG Desktop Portal. You need
`xdg-desktop-portal` **and** a backend matching your desktop:

```bash
# check the service is running
systemctl --user status xdg-desktop-portal

# check a backend is installed
ls /usr/libexec/xdg-desktop-portal-*     # or /usr/lib/xdg-desktop-portal-*
```

Install `xdg-desktop-portal-gnome`, `-kde` or `-wlr` to match your session. On
wlroots compositors, `-wlr` needs a config file naming the output to share.

### Capture fails immediately

```
ERROR openplay_sender::casting: Screen capture failed
```

Usually one of:

- The portal dialog was dismissed or timed out — accept it
- No portal backend, as above
- Running over plain X11 without a portal backend that supports it
- Inside a container or sandbox without the portal socket bound

`detect_session_type()` in `openplay-capture` returns Wayland, X11 or Unknown
from `XDG_SESSION_TYPE`, but nothing calls it at runtime and it logs nothing —
check `echo $XDG_SESSION_TYPE` directly instead.

## Encoding

### No hardware encoder is selected

```
INFO openplay_pipeline::encoder: Selected encoder encoder=x264enc label=x264 (Software) hw=false
```

`hw=false` is what tells you no hardware encoder was chosen.

x264 works but costs noticeably more CPU. Check that GStreamer can actually see
your encoder:

```bash
gst-inspect-1.0 vah264enc     # Intel/AMD VA-API
gst-inspect-1.0 nvh264enc     # NVIDIA NVENC
gst-inspect-1.0 vtenc_h264    # macOS VideoToolbox
gst-inspect-1.0 mfh264enc     # Windows Media Foundation
```

If a factory is missing, install the relevant plugin package. Both `vah264enc`
and `nvh264enc` come from `gstreamer1.0-plugins-bad` (the `va` and `nvcodec`
plugins); VA-API additionally needs a working `libva` driver
(`intel-media-va-driver` or `mesa-va-drivers`), and NVENC needs the NVIDIA
driver. `x264enc` comes from `gstreamer1.0-plugins-ugly`.

Note that OpenPlay does more than check the registry — it also tries to
*instantiate* each candidate, because a factory can be registered while the
underlying device is unavailable. A factory that `gst-inspect-1.0` finds but
OpenPlay skips means instantiation failed, and the log says so:

```
WARN openplay_pipeline::encoder: Encoder found in registry but failed to instantiate
```

That is usually a driver or permissions problem — on Linux, check membership of
the `video` and `render` groups.

### Stream is choppy or the encoder stalls

Try software encoding to isolate the layer:

```toml
force_sw_encode = true
```

If x264 is smooth and hardware is not, the fault is the hardware encoder or its
driver. Lower `max_bitrate_kbps` or `framerate` if x264 is also struggling.

## Discovery

### No receivers appear

mDNS needs UDP 5353 on the local subnet, and does not cross subnets or most VPNs.

```bash
# see what is actually advertised
avahi-browse -a -t
```

OpenPlay looks for:

- `_openplay._tcp.local.` — other OpenPlay receivers, which the receiver binary
  now advertises for itself
- `_airplay._tcp.local.` — AirPlay receivers
- `_display._tcp.local.`, `_miracast._tcp.local.`, `_wfd._tcp.local.` — Miracast

Common causes: a firewall blocking 5353, client isolation on the access point,
being on a VPN, or the devices being on different VLANs. If `avahi-browse` shows
the device and OpenPlay does not, that is a bug worth reporting.

### An OpenPlay receiver does not appear

Check the receiver's own window first. If mDNS registration failed it says
**"Not discoverable on this network — senders must be pointed here by address"**
and keeps listening anyway, which is a different problem from a receiver that
never started.

Then confirm what it is publishing:

```bash
avahi-browse -r _openplay._tcp
```

The TXT record must carry an `fp` key. Without it the sender refuses the
receiver outright with "did not publish a certificate fingerprint" — that is
deliberate, because connecting without a pin would mean trusting whichever host
answered.

## OpenPlay (WebRTC)

### A cast fails instantly and the log says nothing useful

Check the `nice` plugin before anything else:

```bash
gst-inspect-1.0 nice
```

If that prints `No such element or plugin 'nice'`, that is your bug.
`webrtcbin` lives in `gst-plugins-bad`, but its ICE implementation comes from
libnice and is packaged separately everywhere: `gstreamer1.0-nice` on
Debian/Ubuntu, `libnice-gstreamer1` on Fedora, part of `libnice` on Arch. Without
it `webrtcbin` constructs successfully and then **refuses every pad request**, so
nothing links and no error names the cause. See
[install.md](install.md#the-nice-plugin-specifically).

The same applies to `cargo test --all`: the WebRTC loopback test asserts the
plugin is present and fails with that message rather than hanging.

### The sender sits at "Casting..." forever

Working as designed — it is waiting for consent, and there is deliberately no
timeout, because somebody may have to walk across a room. Look at the receiver's
screen: it should be showing an **Allow / Deny** prompt naming the sender. Press
Stop on the sender if you want out; the stop flag is polled every 250 ms even
while the connection is silent.

If the receiver is *not* showing a prompt, the session request never arrived —
work back through TLS and discovery above.

### "Receiver is already showing another device"

One session at a time, because there is one screen. Stop the other cast, or wait
for the receiver's 2-second liveness tick to notice a sender that disappeared
without saying goodbye.

### Connected, but the picture never appears

The receiver shows a spinner and "Receiving from …" once the session connects
but before the first decoded frame. If it stays there, media is not arriving:
check that the sender picked an encoder at all
([above](#no-hardware-encoder-is-selected)), and that nothing is filtering UDP
between the two hosts. Only host ICE candidates are gathered — no
STUN or TURN is contacted — so the two machines must have a direct route to each
other on the LAN.

## AirPlay

### Rejected during pairing

Pairing previously could never succeed — the SRP group was fabricated. That is
fixed, but **has not been confirmed against physical hardware**. If you hit a
pairing failure, `RUST_LOG=openplay_airplay=debug` will show which message it
died on, and that result is worth adding to issue #27 either way.

### "requires FairPlay authentication which is not supported"

Expected on Apple TV 2nd and 3rd generation. OpenPlay reads the model string from
`/info` and refuses those models up front rather than attempting FairPlay, which
is unimplemented. No amount of network debugging will help. See
[crypto.md](crypto.md#fairplay--not-fixed).

### "Receiver returned HTTP 403"

The receiver refused this device before any pairing happened. On macOS this is
the AirPlay Receiver access setting — the default, "Current User", rejects
devices not signed into the same Apple ID. Change it under **System Settings →
General → AirDrop & Handoff → AirPlay Receiver**.

To see exactly how far a handshake gets against a given receiver:

```bash
cargo run -p openplay-airplay --example pair_probe -- <ip>:7000
```

## Miracast

Before debugging anything here: **no cast in this repository has ever been
confirmed against a real Miracast sink.** A failure is at least as likely to be
an OpenPlay bug as a problem with your dongle, and a report saying what happened
is worth more than a workaround.

### The cast ends immediately after the handshake succeeds

This was a bug in OpenPlay, not in any sink: the session emitted `Ended` on the
line after `Ready` and dropped the RTSP socket as soon as M7 completed, so every
cast died in milliseconds. It is fixed — `serve_control_channel` now holds the
control connection open for the life of the cast.

If you still see it, report it. Raise `RUST_LOG=openplay_miracast=debug` and look
for `Holding RTSP control connection open` after the negotiation lines: if that
appears and the cast still stops, the sink ended the session or the connection
broke, and the log will say which.

### Wi-Fi Direct finds no peers (Linux)

First check the D-Bus permission — this is the most common cause and is easy to
miss. OpenPlay talks to wpa_supplicant over the **system** bus, which an
unprivileged user cannot do by default. See
[packaging.md](packaging.md#wi-fi-direct-needs-d-bus-permission) for the two
files to install and the `netdev` group membership required.

Then check `wpa_supplicant` itself, talking to it directly rather than through
NetworkManager:

```bash
systemctl status wpa_supplicant
busctl --system tree fi.w1.wpa_supplicant1     # should not be empty
iw list | grep -A5 "Supported interface modes"  # needs P2P-client / P2P-GO
```

If your adapter does not list P2P modes, it cannot do Wi-Fi Direct.
NetworkManager can also fight wpa_supplicant for the interface.

### Group forms, then nothing streams

After the P2P group forms, **OpenPlay is the RTSP server** and waits on port
7236 for the sink to connect — the source listens, per the WFD spec. It falls
back to connecting outbound after 30 seconds. If both directions fail, check
that 7236 is not firewalled on the P2P interface.

### Wi-Fi Direct on macOS or Windows

Not supported. It requires wpa_supplicant over D-Bus, and the code paths are
gated to Linux. The UI reports this rather than failing silently.

MICE (both devices on the same network) is not gated to Linux, but it has only
ever been exercised there — capture on macOS and Windows is untested.

## Reporting a bug

Include:

- Platform, and for Linux the desktop and session type (Wayland or X11)
- `gst-inspect-1.0 --version`, and for anything WebRTC, `gst-inspect-1.0 nice`
- Which protocol, and whether MICE or Wi-Fi Direct for Miracast
- The receiver model, or for OpenPlay, both machines
- Log output at `RUST_LOG=debug`

For AirPlay and Miracast, whether it works with another sender (UxPlay,
miraclecast) is very useful — it separates "OpenPlay is wrong" from "this
receiver is unusual".
