# OpenPlay

OpenPlay is an open-source screen casting system written in Rust. It lets you cast your screen from any Linux, macOS, or Windows machine to AirPlay receivers, Miracast receivers, or other machines running OpenPlay — all without proprietary software or cloud accounts.

## What it does

OpenPlay ships two binaries:

- **openplay-sender** — captures your screen and streams it to a receiver of your choice.
- **openplay-receiver** — receives and displays incoming streams on a connected display.

Both are standalone GUI applications that discover each other automatically over your local network using mDNS, so there is nothing to manually configure.

## Status

OpenPlay is under active development. This section is the honest summary; the
sections below describe the design, some of which is not yet connected.

**Works today**

- mDNS discovery of AirPlay, Miracast and OpenPlay receivers
- Screen capture on Linux via XDG Desktop Portal and PipeWire
- Hardware encoder probing with x264 fallback
- Miracast sending, including Wi-Fi Direct P2P on Linux
- Configuration loading, validation and first-run file creation

**Partly built**

- **AirPlay sending** — discovery, the HTTP/plist session layer, TLV8, NTP and
  the mirror stream are implemented, but HAP pairing and FairPlay use
  placeholder crypto constants, so real receivers reject the handshake
  ([#8](https://github.com/Developer1010x/openplay/issues/8)).
- **OpenPlay (WebRTC)** — the library pieces exist and are tested
  (`SenderPipeline`, `ReceiverPipeline`, `SignalingServer`, `SignalingClient`,
  `ReceiverAdvertiser`), but neither binary calls them yet. The sender's
  OpenPlay path sets a status string and stops; the receiver window is a static
  "waiting" page ([#11](https://github.com/Developer1010x/openplay/issues/11)).

**Planned**

- AirPlay and Miracast receiver support
- macOS and Windows screen capture backends
- Self-signed TLS certificates generated on first launch. `CertificateManager`
  in `openplay-crypto` is implemented and tested, but nothing constructs it yet.

## Protocol support

| Protocol | Direction | Notes |
|---|---|---|
| AirPlay | Sender only, **not yet interoperable** | Discovery, HTTP/plist session layer, TLV8, NTP and the mirror stream are implemented, but HAP pairing and FairPlay currently use placeholder crypto constants and are rejected by real receivers. Target: Apple TV, AirPlay 2 TVs, and compatible displays. See [#8](https://github.com/Developer1010x/openplay/issues/8) |
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
- Self-signed TLS certificate lifecycle for securing WebRTC connections (implemented in `openplay-crypto`; not yet generated on first launch — see Status)
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
  libpipewire-0.3-dev
```

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

The sender will scan for receivers on your network automatically. Select a receiver from the list, choose a protocol, and click Cast.

### Command-line options

```
openplay-sender
  --config <path>   Use a custom config file
  --name <name>     Override the display name advertised on the network

openplay-receiver
  --config <path>   Use a custom config file
  --name <name>     Override the display name advertised on the network
  --port <port>     Override the signaling port (default: 7290)
```

## Configuration

On first launch, a config file is created at:

- Linux: `$XDG_CONFIG_HOME/openplay/config.toml` (usually `~/.config/openplay/config.toml`)
- macOS: `~/Library/Application Support/openplay/config.toml`
- Windows: `%APPDATA%\openplay\config.toml`

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
  data/                   Desktop entry, AppStream metainfo, icons, D-Bus and polkit files
  flatpak/                Flatpak manifest
```

## Contributing

Contributions are welcome. If you are working on a new feature or a bug fix, open an issue first to discuss the approach. Pull requests should be focused and include a clear description of the change.

Areas where help is particularly useful:

- AirPlay receiver implementation
- Miracast receiver implementation
- macOS and Windows screen capture backends
- Packaging (Flatpak, Homebrew, Winget, AUR)
- Testing against real-world AirPlay and Miracast hardware

## License

OpenPlay is licensed under the GNU General Public License v3.0 or later. See [LICENSE](LICENSE) for the full text.
