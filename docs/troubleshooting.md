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
| Casting via "OpenPlay" does nothing, status flickers and stops | The OpenPlay/WebRTC path is not wired to the binaries |
| Receiver sits on "Waiting for a sender to connect…" forever | The receiver never starts a signaling server |
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

- `_openplay._tcp.local.` — other OpenPlay receivers. Nothing advertises this
  yet, so it never matches
- `_airplay._tcp.local.` — AirPlay receivers
- `_display._tcp.local.`, `_miracast._tcp.local.`, `_wfd._tcp.local.` — Miracast

Common causes: a firewall blocking 5353, client isolation on the access point,
being on a VPN, or the devices being on different VLANs. If `avahi-browse` shows
the device and OpenPlay does not, that is a bug worth reporting.

## AirPlay

### Rejected during pairing

Pairing previously could never succeed — the SRP group was fabricated. That is
fixed, but **has not been confirmed against physical hardware**. If you hit a
pairing failure, `RUST_LOG=openplay_airplay=debug` will show which message it
died on, and that result is worth adding to issue #8 either way.

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
- `gst-inspect-1.0 --version`
- Which protocol, and whether MICE or Wi-Fi Direct for Miracast
- The receiver model
- Log output at `RUST_LOG=debug`

For AirPlay and Miracast, whether it works with another sender (UxPlay,
miraclecast) is very useful — it separates "OpenPlay is wrong" from "this
receiver is unusual".
