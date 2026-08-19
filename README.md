# OpenPlay

[![CI](https://github.com/Developer1010x/openplay/actions/workflows/ci.yml/badge.svg)](https://github.com/Developer1010x/openplay/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Rust 1.80+](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)

OpenPlay is an open-source screen casting system written in Rust. It lets you cast your screen from any Linux, macOS, or Windows machine to AirPlay receivers, Miracast receivers, or other machines running OpenPlay — all without proprietary software or cloud accounts.

## What it does

OpenPlay ships two binaries:

- **openplay-sender** — captures your screen and streams it to a receiver of your choice.
- **openplay-receiver** — receives and displays incoming streams on a connected display.

The sender finds receivers on the local network over mDNS, so there are no IP addresses to type. The OpenPlay receiver does not yet advertise itself or run a signaling server, so two OpenPlay instances cannot connect to each other yet — see Status.

## Status

OpenPlay is under active development. This section is the honest summary; the
sections below describe the design, some of which is not yet connected.

**Works today**

- mDNS discovery of AirPlay and Miracast receivers (the sender also browses for
  OpenPlay receivers, but nothing advertises that service yet)
- Screen capture on Linux via XDG Desktop Portal and PipeWire
- Hardware encoder probing with x264 fallback
- Miracast sending, including Wi-Fi Direct P2P on Linux
- Configuration loading, validation and first-run file creation

**Partly built**

- **AirPlay sending** — discovery, the HTTP/plist session layer, TLV8, NTP, the
  mirror stream and HAP pairing are implemented. Pairing previously used a
  fabricated SRP group and could never succeed; it now uses the real RFC 5054
  3072-bit group, but is unconfirmed against physical hardware
  ([#27](https://github.com/Developer1010x/openplay/issues/27)). FairPlay will
  not be implemented here, so Apple TV 2nd/3rd generation are refused up front
  with an explicit error — see the decision in
  [docs/crypto.md](docs/crypto.md).
- **OpenPlay (WebRTC)** — the library pieces exist (`SenderPipeline`,
  `ReceiverPipeline`, `SignalingServer`, `SignalingClient`,
  `ReceiverAdvertiser`) but have no test coverage, and neither binary calls
  them. The sender's OpenPlay path sets a status string and stops; the receiver
  window is a static "waiting" page
  ([#11](https://github.com/Developer1010x/openplay/issues/11)).

**Not implemented at all**

- **Audio.** OpenPlay casts video only. There is no audio capture, encoding or
  transport anywhere in the workspace. Note this despite the protocol layer
  advertising Opus in `Capabilities` and Miracast negotiating `WfdAudioCodecs` —
  those are declarations the pipeline does not honour.

**Planned**

- AirPlay and Miracast receiver support
- macOS and Windows screen capture backends
- Self-signed TLS certificates generated on first launch. `CertificateManager`
  in `openplay-crypto` is implemented and tested, but nothing constructs it yet.

## Protocol support

| Protocol | Direction | Notes |
|---|---|---|
| AirPlay | Sender only, **untested against hardware** | Discovery, HTTP/plist session layer, TLV8, NTP, the mirror stream and HAP pairing are implemented. FairPlay will not be implemented, so receivers that require it (Apple TV 2nd/3rd gen) are rejected by design. Target: Apple TV, AirPlay 2 TVs, and compatible displays. See [#27](https://github.com/Developer1010x/openplay/issues/27) |
| Miracast / Wi-Fi Display | Sender only | Cast to Miracast adapters and smart TVs; Wi-Fi Direct P2P supported on Linux |
| OpenPlay (WebRTC) | Sender and receiver, **not yet wired up** | Native protocol between two OpenPlay instances. The signaling, pipeline and discovery libraries are implemented; connecting them to the two binaries is in progress. See [#11](https://github.com/Developer1010x/openplay/issues/11) |

AirPlay receiver support and Miracast receiver support are planned for a future release.

## Features

- Auto-discovery of receivers on the local network via mDNS — no IP addresses to type
- Hardware-accelerated H.264 encoding with automatic fallback to software
  - Linux: VA-API (Intel/AMD), NVENC (NVIDIA)
  - macOS: VideoToolbox
  - Windows: Media Foundation, NVENC
  - All platforms: x264 software fallback
- Screen capture via XDG Desktop Portal and PipeWire on Linux
- Configurable bitrate and framerate
- Self-signed certificate lifecycle for securing WebRTC connections (implemented in `openplay-crypto`; nothing constructs it yet, so no certificate is generated on first launch — see Status)
- Lightweight egui GUI — no Electron, no browser runtime

## Building from source

### Prerequisites

- Rust 1.80 or later
- GStreamer 1.22 or later, including the following plugins:
  - `gstreamer`, `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`, `gst-plugin-webrtc`
- On Linux: PipeWire and the XDG Desktop Portal (`xdg-desktop-portal` and a backend such as `xdg-desktop-portal-gnome` or `xdg-desktop-portal-wlr`)
- On Linux (Miracast Wi-Fi Direct): `wpa_supplicant` with D-Bus support

**Ubuntu / Debian:**

```bash
sudo apt install \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libgstreamer-plugins-bad1.0-dev \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-libav \
  gstreamer1.0-pipewire \
  libpipewire-0.3-dev
```

`gstreamer1.0-pipewire` provides the `pipewiresrc` element that Linux capture
feeds into; `libpipewire-0.3-dev` alone is not enough at runtime.

**Fedora:**

```bash
sudo dnf install \
  gstreamer1-devel \
  gstreamer1-plugins-base-devel \
  gstreamer1-plugins-bad-free-devel \
  gstreamer1-plugins-good \
  pipewire-devel
```

**macOS (Homebrew):**

```bash
brew install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad
```

### Compile

```bash
git clone https://github.com/Developer1010x/openplay.git
cd openplay
cargo build --release
```

The compiled binaries will be at `target/release/openplay-sender` and `target/release/openplay-receiver`.

## Running

**Start the sender** on the machine whose screen you want to cast:

```bash
./openplay-sender
```

**Start the receiver** on the machine that will display the stream:

```bash
./openplay-receiver
```

The sender scans for receivers automatically. Select one from the list — its protocol badge (AirPlay, Miracast or OpenPlay) determines how it is cast to, there is no separate protocol chooser — then click **▶ Start Casting**.

### Command-line options

```
openplay-sender
  --config <path>   Use a custom config file
  --name <name>     Override the display name shown in the window

openplay-receiver
  --config <path>   Use a custom config file
  --name <name>     Override the display name (not yet advertised over mDNS)
  --port <port>     Override the signaling port (default: 7290)
```

## Configuration

On first launch, a config file is created at:

- Linux: `$XDG_CONFIG_HOME/openplay/config.toml` (usually `~/.config/openplay/config.toml`)
- macOS: `~/Library/Application Support/org.openplay.OpenPlay/config.toml`
- Windows: `%APPDATA%\openplay\OpenPlay\config\config.toml`

Example `config.toml`:

```toml
display_name = "My Laptop"
port = 7290
max_bitrate_kbps = 6000
framerate = 30
force_sw_encode = false
airplay_enabled = true
miracast_enabled = true
```

Configuration values are validated against supported ranges on load: the display name must be non-empty, the port non-zero, `max_bitrate_kbps` between 100 and 100000, and `framerate` between 1 and 240. An out-of-range value (for example a `max_bitrate_kbps` of `0` from a typo) is reported as a clear configuration error rather than failing later inside the media pipeline.

## Repository layout

A flat Cargo workspace of eleven crates. The two binaries are
`openplay-sender` and `openplay-receiver`; everything else is a library used by
one or both.

```
openplay/
  crates/
    openplay-sender/      Binary: sender GUI (egui), receiver list, casting logic
    openplay-receiver/    Binary: receiver GUI (egui), display window
    openplay-airplay/     AirPlay protocol: HAP pairing, FairPlay, NTP, mirror stream, TLV8
    openplay-miracast/    Miracast / Wi-Fi Display: RTSP, WFD params, Wi-Fi Direct (Linux)
    openplay-signaling/   WebSocket signaling client and server
    openplay-protocol/    OpenPlay signaling wire format and state machine
    openplay-discovery/   mDNS advertisement and browsing
    openplay-pipeline/    GStreamer pipeline construction and encoder probing
    openplay-capture/     Screen capture abstraction (XDG Portal / PipeWire on Linux)
    openplay-crypto/      Self-signed TLS certificate lifecycle
    openplay-common/      Configuration, logging, XDG paths, shared constants
  data/                   Desktop entry, AppStream metainfo, icon, D-Bus and polkit files
  flatpak/                Flatpak manifest
  docs/                   Install, configuration, architecture, protocols, crypto,
                          packaging, troubleshooting, contributing
  .github/workflows/      CI
```

Each crate also carries its own `README.md`. Full documentation is in
[docs/](docs/README.md).

## Contributing

**Contributions are very welcome, and the project is early enough that there is a
lot of well-scoped work available.**

Start with [CONTRIBUTING.md](CONTRIBUTING.md) — it gets you building in about a
minute — or go straight to the
[good first issues](https://github.com/Developer1010x/openplay/labels/good%20first%20issue).

The most useful thing most people can do costs nothing to try: **run it against
real hardware and report what happened.** No test in this repository can
substitute for an actual Apple TV or Miracast dongle, and a failure report is
just as valuable as a success.

Where help goes furthest:

| Area | Difficulty |
|---|---|
| Testing against real AirPlay / Miracast hardware | Easy |
| Documentation corrections | Easy |
| Wiring the OpenPlay/WebRTC path to the binaries | Medium |
| Audio support — there is none today | Medium |
| Verifying macOS and Windows capture | Medium |
| [Receiving AirPlay on Linux](docs/airplay-receiver-design.md) | Hard |

Open an issue before a large change so the approach can be agreed first. Small
fixes can go straight to a pull request. No CLA.

## License

OpenPlay is licensed under the GNU General Public License v3.0 or later. See [LICENSE](LICENSE) for the full text.
