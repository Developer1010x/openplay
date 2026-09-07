# Installing and running

There are no packaged releases yet, so this means building from source. CI
publishes Linux release binaries as build artifacts, but they are not signed and
not a distribution channel.

## What works on which platform

Worth knowing before you spend time on the build:

| Platform | Sender | Receiver |
|---|---|---|
| Linux | OpenPlay/WebRTC and Miracast wired end to end but unverified in the field; AirPlay partly (see below) | Advertises, listens, prompts for consent and displays video — never verified against a separate machine |
| macOS | Capture untested, and not built by CI | Not built by CI; untested |
| Windows | Capture untested, and not built by CI | Not built by CI; untested |

"Wired" here means every call path exists and in-process tests exercise it. It
does **not** mean anyone has reported a successful cast between two machines, or
against a real Miracast sink. If you run one, say so in an issue either way —
that report is currently the missing piece.

Note what the macOS and Windows rows do *not* say. CI's cross-platform job
checks only the crates that build without GStreamer or PipeWire, and
`openplay-sender`, `openplay-receiver` and `openplay-pipeline` are explicitly
excluded from it — they are built on Linux and nowhere else, so on those
platforms even compilation is unattested. See
[contributing.md](contributing.md#ci-jobs).

Only Linux capture has been exercised, via the XDG Desktop Portal and PipeWire.
On macOS and Windows capture is left to GStreamer's own elements
(`screencapturesrc`/`avfvideosrc` and `d3d11screencapturesrc`) and that path has
never been verified. `CaptureSession` also only *claims* to report the primary
display size there: `query_primary_display_size()` queries `GetSystemMetrics` on
Windows, but its macOS branch is an empty block with a comment, so macOS gets a
hardcoded 1920x1080 regardless of the panel. Until recently the Windows build
did not compile at all — `openplay-capture` used the `windows` crate without
declaring it — which is a fair indication of how untested it is.

See the README Status section for the current picture, and its Security model
section before exposing a receiver to a network you do not control.

## Prerequisites

- Rust **1.88** or later. Note that `rust-version` in the root `Cargo.toml` says
  1.80 and is wrong by eight minor versions: 1.80 fails at dependency
  resolution, before compiling anything, because `zvariant_utils` needs the
  `edition2024` cargo feature (1.85+) and `time` and the `zbus` 5.x crates
  require 1.87–1.88. CI pins 1.88.0 in its `MSRV` job, which is the number
  actually enforced
- GStreamer 1.22 or later, with `gst-plugins-base`, `gst-plugins-good`,
  `gst-plugins-bad` (`webrtcbin`, `h264parse`), `gst-plugins-ugly` (for the
  `x264enc` fallback), the GStreamer PipeWire plugin (for `pipewiresrc` on
  Linux), and **the `nice` plugin from libnice**
- Linux: PipeWire, and `xdg-desktop-portal` plus a backend
  (`xdg-desktop-portal-gnome`, `-kde` or `-wlr`)
- Linux, for Miracast Wi-Fi Direct: `wpa_supplicant` with D-Bus support, **plus
  the D-Bus and polkit files from `data/` and membership of the `netdev` group** —
  see [packaging.md](packaging.md#wi-fi-direct-needs-d-bus-permission). Without
  this, Wi-Fi Direct silently finds no peers

### The `nice` plugin, specifically

Read this one before filing a WebRTC bug. `webrtcbin` lives in
`gst-plugins-bad`, but the ICE implementation it needs lives in **libnice** and
is packaged separately on every distribution. Installing `gst-plugins-bad` alone
is not enough.

The failure mode is nasty because nothing errors early: `webrtcbin` constructs
fine, and then **refuses every `sink_%u` pad request**, so the pipeline never
links and a cast dies with no message that names the cause. This was hit for
real during the WebRTC work, on a machine where every other GStreamer package
was already installed, which is why it gets a section of its own.

Check before anything else:

```bash
gst-inspect-1.0 nice        # must print a plugin with nicesrc and nicesink
```

`No such element or plugin 'nice'` means install the package for your
distribution below. The same requirement applies to
`cargo test --all`: `crates/openplay-pipeline/tests/webrtc_loopback.rs` asserts
the plugin is present up front, so the suite fails loudly rather than obscurely
on a machine without it.

### Ubuntu / Debian

```bash
sudo apt install \
  libgstreamer1.0-dev \
  libgstreamer-plugins-base1.0-dev \
  libgstreamer-plugins-bad1.0-dev \
  gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly \
  gstreamer1.0-nice \
  gstreamer1.0-libav \
  gstreamer1.0-pipewire \
  libpipewire-0.3-dev
```

The `-dev` packages and the plugin packages are **not** interchangeable, and the
overlap in their names hides that. `libgstreamer-plugins-base1.0-dev` gives you
headers and `libgstreamer-plugins-base1.0-0`; the `appsink` and `videoconvert`
*factories* come from `gstreamer1.0-plugins-base`, which the dev package does not
depend on. List it explicitly rather than relying on some other plugin package
happening to pull it in.

What each of the rest supplies:

- `gstreamer1.0-plugins-good` — `rtph264pay` / `rtph264depay`
- `gstreamer1.0-plugins-bad` — `webrtcbin` and `h264parse`; the *runtime* twin of
  `libgstreamer-plugins-bad1.0-dev`, not a substitute for it
- `gstreamer1.0-plugins-ugly` — `x264enc`, the universal software encoder
- `gstreamer1.0-libav` — `avdec_h264`, the software decode fallback the receiver
  needs on any machine without a working VA-API or NVENC decoder
- `gstreamer1.0-nice` — the ICE plugin, from the `libnice` source package; on
  Ubuntu it lives in **universe**
- `gstreamer1.0-pipewire` — `pipewiresrc`, which Linux capture feeds into.
  `libpipewire-0.3-dev` alone is not enough at runtime

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
  gstreamer1-plugins-base \
  gstreamer1-plugins-bad-free \
  gstreamer1-plugins-good \
  gstreamer1-plugins-ugly-free \
  libnice-gstreamer1 \
  pipewire-gstreamer \
  pipewire-devel \
  xdg-desktop-portal
```

Two Fedora-specific things:

**The `nice` plugin is `libnice-gstreamer1`**, from the `libnice` source
package. It is **not** part of `gstreamer1-plugins-bad-free`, so installing the
bad-plugins package does not get you `webrtcbin`'s ICE backend.

**`avdec_h264` is not in Fedora proper.** It comes from `gstreamer1-libav`,
which lives in RPM Fusion (free). That matters on the receive side only, and
only where hardware decode is unavailable: `build_decoder_element()` tries
`vah264dec` then `nvh264dec`, and its single software fallback is `avdec_h264`
— there is no openh264 path. A Fedora receiver with a working VA-API driver
never notices; one without it fails to build a decode chain at all. Install
`gstreamer1-libav` from RPM Fusion if you need the fallback.

### Arch

```bash
sudo pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
  gst-plugins-ugly gst-libav gst-plugin-pipewire libnice pipewire \
  xdg-desktop-portal
```

On Arch the plugin is inside the base `libnice` package
(`/usr/lib/gstreamer-1.0/libgstnice.so`) rather than a separate one.

### macOS

```bash
brew install gstreamer
```

Homebrew folded `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`,
`gst-plugins-ugly`, `gst-libav` and the rest into the single `gstreamer`
formula, so those names now just resolve back to it — installing `gstreamer` is
the whole list. That formula depends on `libnice`, but confirm with
`gst-inspect-1.0 nice` rather than assuming, and confirm `x264enc` the same way.
Builds, but capture is untested — see the table above.

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

Starts the receiver: it generates a TLS certificate on first launch (in the data
directory — see [configuration.md](configuration.md#data-directory)), registers
itself over mDNS with that certificate's fingerprint, binds the signaling port
from the config, and sits on the waiting screen.

When a sender asks to cast, the receiver shows an **Allow / Deny** prompt naming
the sender. Nothing is accepted, no pipeline is built and no pixel is displayed
until somebody presses Allow — there is no timeout that accepts, and one session
runs at a time. That prompt is the only access control there is: there is no
pairing and no authentication behind it, and the mDNS fingerprint is
unauthenticated. See the README's Security model section.

If mDNS registration fails, the receiver says so on screen and keeps listening —
a sender pointed at the address by hand still works.

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

## Debian package

```bash
cargo build --release
packaging/build-deb.sh          # writes dist/openplay_<version>_<arch>.deb
sudo apt install ./dist/openplay_0.1.0_amd64.deb
```

The script stages the two binaries plus everything under `data/` (desktop entry,
AppStream metainfo, icon, and the D-Bus and polkit files Wi-Fi Direct needs), and
computes the library `Depends` with `dpkg-shlibdeps` against the binaries it is
about to ship, so that list cannot drift.

Two sets of dependencies are listed by hand instead, because everything in them
is dlopen'd and leaves no ELF `NEEDED` entry for shlibdeps to find: the
**GStreamer plugin packages** (element factories, including `gstreamer1.0-nice`)
and the **GUI libraries** eframe/winit/glutin open through `libloading`. Omitting
either produces the same nasty shape of failure — a package that installs
cleanly and then fails at runtime.

**These lists and the ones above must stay in step.** If you change the plugin
requirements here, change `packaging/build-deb.sh` to match, and vice versa:

```bash
grep -n 'gst_deps=\|gui_deps=' packaging/build-deb.sh
```

CI builds this on every run in the `Build Release` job and attaches it as the
`openplay-deb` artifact. It is not published to any apt repository.

There is no macOS `.dmg` job in CI — that build is done by hand on a Mac, which
keeps CI on the free Linux runners.

## Flatpak

A manifest exists at `flatpak/org.openplay.OpenPlay.yml`, along with a desktop
entry, AppStream metainfo, D-Bus and polkit files under `data/`. **The manifest
does not build yet** — it fetches crates with `cargo --offline` inside a sandbox
that has no network and no vendored sources — and it is not published to a
remote. See [packaging.md](packaging.md#flatpak).
