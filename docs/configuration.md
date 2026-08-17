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
display_name     = "My Laptop"   # shown in the UI (mDNS advertising not wired up)
port             = 7290          # signaling port (receiver only)
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

**`port`** is the OpenPlay signaling port and matters only to the receiver.
AirPlay uses 7000 and Miracast RTSP uses 7236; neither is configurable here.
Port 0 is rejected — it means "any free port" to the OS, which makes a receiver
effectively undiscoverable.

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
  --name <name>     Override the display name (not yet advertised over mDNS)
  --port <port>     Override the signaling port (default: 7290)
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
returns the same directory for both. Used for the paired-device database and the TLS
certificate and key once that path is wired up:

- Linux: `$XDG_DATA_HOME/openplay/` (usually `~/.local/share/openplay/`)
- macOS and Windows: the `directories` crate's data directory for the same
  qualifier
