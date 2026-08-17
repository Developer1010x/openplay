# Installing and running

There are no packaged releases yet, so this means building from source. CI
publishes Linux release binaries as build artifacts, but they are not signed and
not a distribution channel.

## What works on which platform

Worth knowing before you spend time on the build:

| Platform | Sender | Receiver |
|---|---|---|
| Linux | Miracast works; AirPlay partly (see below); OpenPlay/WebRTC not wired up | Not wired up |
| macOS | Builds; capture untested | Not wired up |
| Windows | Builds; capture untested | Not wired up |

Only Linux capture has been exercised, via the XDG Desktop Portal and PipeWire.
On macOS and Windows `CaptureSession` merely reports the primary display size and
capture is left to GStreamer's own elements (`screencapturesrc`/`avfvideosrc` and
`d3d11screencapturesrc`); that path has never been verified. Until recently the
Windows build did not compile at all — `openplay-capture` used the `windows`
crate without declaring it — which is a fair indication of how untested it is.

The receiver binary does not host a session on any platform.

See the README Status section for the current picture.

## Prerequisites

- Rust 1.80 or later
- GStreamer 1.22 or later, with `gst-plugins-base`, `gst-plugins-good`,
  `gst-plugins-bad`, `gst-plugins-ugly` (for the `x264enc` fallback), the
  GStreamer PipeWire plugin (for `pipewiresrc` on Linux) and `gst-plugin-webrtc`
- Linux: PipeWire, and `xdg-desktop-portal` plus a backend
  (`xdg-desktop-portal-gnome`, `-kde` or `-wlr`)
- Linux, for Miracast Wi-Fi Direct: `wpa_supplicant` with D-Bus support, **plus
  the D-Bus and polkit files from `data/` and membership of the `netdev` group** —
  see [packaging.md](packaging.md#wi-fi-direct-needs-d-bus-permission). Without
  this, Wi-Fi Direct silently finds no peers

### Ubuntu / Debian

```bash
sudo apt install \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libgstreamer-plugins-bad1.0-dev \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly \
  gstreamer1.0-libav \
  gstreamer1.0-pipewire \
  libpipewire-0.3-dev
```

`gstreamer1.0-pipewire` supplies `pipewiresrc`, which Linux capture feeds into —
`libpipewire-0.3-dev` alone is not enough at runtime. `gstreamer1.0-plugins-ugly`
supplies `x264enc`, the universal software fallback.

For hardware encoding, both `vah264enc` and `nvh264enc` come from
`gstreamer1.0-plugins-bad`; VA-API also needs a working `libva` driver
(`intel-media-va-driver` or `mesa-va-drivers`) and NVENC needs the NVIDIA
driver.

### Fedora

```bash
sudo dnf install \
  gstreamer1-devel \
  gstreamer1-plugins-base-devel \
  gstreamer1-plugins-bad-free-devel \
  gstreamer1-plugins-good \
  gstreamer1-plugins-ugly-free \
  pipewire-gstreamer \
  pipewire-devel \
  xdg-desktop-portal
```

### Arch

```bash
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
  gst-plugins-ugly gst-libav gst-plugin-pipewire pipewire xdg-desktop-portal
```

### macOS

```bash
brew install gstreamer
```

Recent Homebrew folds the `gst-plugins-*` formulae into `gstreamer`; check the
current formula names, and make sure whichever one supplies `x264enc` is
installed. Builds, but capture is untested — see the table above.

## Building

```bash
git clone https://github.com/Developer1010x/openplay.git
cd openplay
cargo build --release
```

Binaries land at `target/release/openplay-sender` and
`target/release/openplay-receiver`.

## Running

```bash
./target/release/openplay-sender
```

The sender scans the local network automatically. Select a receiver from the list
— its protocol badge (AirPlay, Miracast or OpenPlay) determines how it is cast
to, there is no separate protocol chooser — then click **▶ Start Casting**.

On Linux *every* cast raises an XDG Desktop Portal dialog asking which screen to
share. This is the desktop's own permission prompt, the choice is not persisted
between casts, and OpenPlay requests monitors only (not individual windows).

```bash
./target/release/openplay-receiver
```

Starts the receiver UI. It does not yet host a session, so it will sit on the
waiting screen.

### Verifying your setup without a receiver

The encoder is only probed once a cast starts, so you need something in the list
first. Use **+ Miracast IP** to add any reachable address, then:

```bash
RUST_LOG=openplay_pipeline=debug ./target/release/openplay-sender
```

The log reports the selected encoder on the first cast attempt:

```
INFO openplay_pipeline::encoder: Selected encoder encoder=vah264enc label=... hw=true
```

`hw=false` means it fell back to x264 — see
[troubleshooting.md](troubleshooting.md#no-hardware-encoder-is-selected).

## Flatpak

A manifest exists at `flatpak/org.openplay.OpenPlay.yml`, along with a desktop
entry, AppStream metainfo, D-Bus and polkit files under `data/`. It is not yet
published to a remote.
