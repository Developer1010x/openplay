# Configuration

## Where the file lives

Written on first launch, at the platform config directory:

| Platform | Path |
|---|---|
| Linux | `$XDG_CONFIG_HOME/openplay/config.toml` (usually `~/.config/openplay/config.toml`) |
| macOS | `~/Library/Application Support/org.openplay.OpenPlay/config.toml` |
| Windows | `%APPDATA%\openplay\OpenPlay\config\config.toml` |

Paths come from the `directories` crate with qualifier `org`, organisation
`openplay`, application `OpenPlay`, so the exact macOS and Windows layout
follows that crate's conventions rather than being hardcoded.

Override the location entirely with `--config <path>` on either binary.

If the file does not exist, the defaults are written to it and used. If the
directory is read-only, that failure is logged and the application continues
with in-memory defaults rather than refusing to start. If the file exists but
does not parse or does not validate, that **is** fatal — you asked for something
specific and it cannot be honoured silently.

## Keys

```toml
display_name     = "My Laptop"   # shown in the UI, and advertised over mDNS
port             = 7290          # signaling port the receiver binds
max_bitrate_kbps = 6000          # video bitrate
framerate        = 30            # target frames per second
force_sw_encode  = false         # skip hardware encoder probing
airplay_enabled  = true          # enable AirPlay support
miracast_enabled = true          # enable Miracast support
```

| Key | Type | Default | Valid range |
|---|---|---|---|
| `display_name` | string | system hostname | non-empty after trimming |
| `port` | integer | 7290 | 1–65535 (0 rejected) |
| `max_bitrate_kbps` | integer | 6000 | 100–100000 |
| `framerate` | integer | 30 | 1–240 |
| `force_sw_encode` | boolean | false | — |
| `airplay_enabled` | boolean | true | — |
| `miracast_enabled` | boolean | true | — |

Missing keys take their default because the struct is `#[serde(default)]`, and
unknown keys are ignored because it does not set `#[serde(deny_unknown_fields)]`.

### Notes on individual keys

**`display_name`** matters on both ends, and on the sender it is a security
setting rather than a cosmetic one. On the receiver it is the window title and
the name published in the mDNS TXT record, so it is what appears in the sender's
receiver list. On the sender it is the name sent in `SessionRequest`, which is
the name on the receiver's **Allow / Deny** prompt — the whole basis on which a
person decides to let the cast through. Set it to something a human at the far
end will recognise.

It is sanitised, not trusted: control characters are stripped and the value is
truncated to the protocol's `MAX_NAME_CHARS`, so a misconfigured name becomes a
shortened one rather than a rejected session. An empty or blank name falls back
to "OpenPlay Sender", which is exactly the uninformative prompt worth avoiding.

**`port`** is the OpenPlay signaling port and matters only to the receiver,
which binds it at startup: `[::]:port` where dual-stack works, falling back to
`0.0.0.0:port`. It is also the port published in the mDNS record, so senders
find it without being told. AirPlay uses 7000 and Miracast RTSP uses 7236;
neither is configurable here. Port 0 is rejected — it means "any free port" to
the OS, which makes a receiver effectively undiscoverable.

**`max_bitrate_kbps` and `framerate`** are read on both ends of an OpenPlay
session, but only the sender's values actually take effect. The receiver caps the
sender's requested framerate at its own and returns both in `NegotiatedParams`;
the sender logs the accepted codec and then builds its pipeline from its own
config regardless. Negotiation is reported, not yet applied.

**`max_bitrate_kbps`** has an upper bound of 100000 (100 Mbps) as a typo guard.
A screen cast never needs that much, so a larger value almost always means bytes
were confused for kilobits.

**`force_sw_encode`** makes the sender skip GStreamer registry probing entirely
and use x264. It exists for debugging hardware-encoder problems — if casting
works with this set and fails without it, the fault is in the hardware encoder
or its driver, not in OpenPlay's pipeline construction.

## Validation

Values are checked on load, and **again after command-line overrides are
applied**. That second check is what catches `--port 0` and `--name ""`, which
bypass the file entirely.

An invalid value is reported at startup as a configuration error naming the
field and the accepted range:

```
Error: Configuration error: max_bitrate_kbps must be between 100 and 100000, got 0
```

rather than surfacing later as an opaque GStreamer failure.

## Command-line options

```
openplay-sender
  --config <path>   Use a custom config file
  --name <name>     Override the display name shown in the window

openplay-receiver
  --config <path>   Use a custom config file
  --name <name>     Override the display name (window and mDNS record)
  --port <port>     Override the signaling port the receiver binds (default: 7290)
```

Overrides apply on top of the file and are validated; they are not persisted
back to it.

## Logging

Both binaries use `tracing` and honour `RUST_LOG`, defaulting to `info`:

```bash
RUST_LOG=debug openplay-sender
RUST_LOG=openplay_pipeline=debug,openplay_airplay=trace openplay-sender
RUST_LOG=openplay_airplay=debug openplay-sender     # AirPlay handshake detail
```

Target names use underscores, matching the crate names.

## Data directory

Separate from config on Linux and Windows — on macOS the `directories` crate
returns the same directory for both. Holds the paired-device database and the
receiver's TLS certificate and key, which `CertificateManager::load_or_generate`
writes there on first launch:

- Linux: `$XDG_DATA_HOME/openplay/` (usually `~/.local/share/openplay/`)
- macOS and Windows: the `directories` crate's data directory for the same
  qualifier

Deleting it makes the receiver generate a fresh certificate on the next launch,
with a new fingerprint. That is a supported way to reset the receiver's
identity; senders will simply pin the new value, because there is no pairing
that would notice the change.

Logging at `RUST_LOG=debug` is also the fastest way to see the OpenPlay
handshake — `openplay_receiver::net` reports the consent decision, and
`openplay_pipeline::webrtc` reports each SDP and ICE step.
