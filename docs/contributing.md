# Contributing

Open an issue before starting a feature or a non-obvious fix, so the approach
can be agreed first. Pull requests should be focused and describe what changed
and why.

## Setting up

See [install.md](install.md) for the system dependencies — the GStreamer
development packages are required to build, and on Linux so are the PipeWire
headers; `xdg-desktop-portal` and a backend are needed at runtime.

```bash
git clone https://github.com/Developer1010x/openplay.git
cd openplay
cargo build
```

One dependency will bite you at *test* time rather than build time: the `nice`
GStreamer plugin. `crates/openplay-pipeline/tests/webrtc_loopback.rs` asserts it
is installed before doing anything, because without it `webrtcbin` builds fine
and then refuses every pad request. Check with `gst-inspect-1.0 nice`, and
install `gstreamer1.0-nice` (Debian/Ubuntu), `libnice-gstreamer1` (Fedora) or
`libnice` (Arch) if it is missing. See
[install.md](install.md#the-nice-plugin-specifically).

## The commands CI runs

Run these before pushing. CI runs the same commands, but split across jobs:
`fmt --check` then `clippy` in one job (a fmt failure stops clippy), `cargo test
--all` in a separate job, and the release build gated on both.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build --release
```

Two notes worth knowing:

**Formatting gates linting.** `cargo fmt --check` runs first, so a formatting
failure hides every clippy finding behind it. This is how a batch of clippy
errors sat undiscovered in this repository — the fmt step had never passed.

**Clippy needs every target to compile.** `--all-targets` includes test targets,
and clippy cannot lint a target that fails to build. It also stops scheduling
work once a crate fails, so a reported error count is a floor, not a total. If
you fix a compile error, re-run clippy before assuming you know how much is
left.

### Running a subset

```bash
cargo test -p openplay-protocol
cargo test -p openplay-airplay
cargo test -p openplay-protocol test_serialize_session_request   # single test
cargo clippy -p openplay-miracast --all-targets --all-features -- -D warnings
```

The two integration tests that cover the OpenPlay path end to end are worth
knowing by name, because they are the only thing standing in for a second
machine:

```bash
cargo test -p openplay-signaling --test loopback      # TLS + WebSocket + framing
cargo test -p openplay-pipeline  --test webrtc_loopback   # two webrtcbins, real frames
```

The second needs GStreamer at runtime, including the `nice` plugin. The first
needs neither.

## CI jobs

| Job | Runs on | Covers |
|---|---|---|
| `Check & Lint` | ubuntu-24.04 | `fmt --check`, then `clippy -D warnings` |
| `Test (ubuntu-24.04)` | ubuntu-24.04 | `cargo test --all` |
| `Cross-platform check (macos-14 / windows-2022)` | macos-14, windows-2022 | `cargo check --locked --all-targets` on the portable crates |
| `MSRV (1.88.0)` | ubuntu-24.04 | `cargo check --locked --all-targets` on the same crates, on the oldest toolchain that can build them |
| `Build Release` | ubuntu-24.04 | `cargo build --release`, uploads binaries and the `.deb` |

The cross-platform and MSRV jobs share one crate selection, expressed as
`--workspace --exclude openplay-pipeline --exclude openplay-sender --exclude
openplay-receiver` rather than as a list of names. That matters: a **new crate
is covered automatically**, and only a crate that needs GStreamer or PipeWire
should ever be added to the exclusions. Today the selection is
`openplay-common`, `-protocol`, `-crypto`, `-capture`, `-discovery`,
`-signaling`, `-airplay` and `-miracast`. The MSRV job needs no system packages
at all, which makes it the cheapest job here.

**The Linux jobs' system packages come from `.github/actions/linux-deps`, and it
installs two kinds of package that are not interchangeable.** A `-dev` package
supplies the pkg-config file and headers a `*-sys` crate links against; the
runtime plugin package supplies the `.so` GStreamer loads from its registry.
Neither substitutes for the other, and `libgstreamer-plugins-bad1.0-dev` and
`gstreamer1.0-plugins-bad` really are different packages with the same job on
opposite sides of the divide.

So there are two things to check whenever `openplay-pipeline` changes: that the
action installs headers for every `gstreamer-*` binding a crate declares, and
that it installs the runtime plugins any test calling `gstreamer::init()` will
look for. A missing `-dev` package fails the build; a missing plugin fails at
`ElementFactory::make()` with a `MissingElement` error, which is the failure that
passes on your machine and not on CI, or the reverse. The action's header
comment records which element comes from which package and why each is there —
read it before adding or removing a line.

`docs/install.md` and that list answer different questions and must not be
trimmed to match each other: this file's list is what CI needs to compile and
run `cargo test --all`, while `install.md` is what a user needs to *run* the
app, which is more.

`openplay-pipeline`, `-sender` and `-receiver` need GStreamer and are **only
built on Linux**. If you change platform-gated code in those three, CI will not
catch a break on macOS or Windows. Say so in the PR.

You cannot substitute `cargo check --target` from Linux for those runners:
`ring` and `rusqlite` compile C that a Linux `cc` will not build for macOS or
Windows hosts, so the run fails for reasons unrelated to your change. It *is*
valid for crates with no C dependencies, which is how the `openplay-capture`
Windows break was reproduced locally.

## Platform-gated code

Miracast Wi-Fi Direct and PipeWire capture are Linux-only. Gating a module in
`lib.rs` is not sufficient:

- gate the `mod` declaration
- gate every `use` of it, **including imports only the gated code needs** —
  otherwise the fix trades a hard error for unused-import warnings, which are
  errors under `-D warnings`
- gate every caller, and give non-Linux a sensible branch where the UI would
  otherwise silently do nothing

To check the non-Linux path of a C-dependency-free crate without a Mac,
temporarily rewrite the cfg value to one that never matches and lint with the
resulting noise suppressed:

```bash
sed -i 's/target_os = "linux"/target_os = "notlinux"/g' crates/openplay-miracast/src/*.rs
cargo clippy -p openplay-miracast --all-targets --all-features -- -D warnings -A unexpected_cfgs
# revert when done
```

## Conventions

**Workspace dependencies.** Every internal crate reference uses
`{ workspace = true }`. Path mappings live once in the root `Cargo.toml`. When
adding or moving a crate, edit only the root manifest.

**Never hardcode an encoder.** Use `probe_best_encoder()`. The only bypass is
the `force_sw_encode` config flag, handled in `select_encoder()`.

**Session status flows over channels**, not return values — see
[architecture.md](architecture.md#session-events-travel-over-channels).

**Config is validated after CLI overrides**, not only at load. If you add a flag
that overrides a config field, make sure `validate()` still runs after it.

**The MSRV is real, and it is 1.88 — not the 1.80 in `Cargo.toml`.** Every crate
inherits `rust-version` from the workspace, and that field says 1.80, but 1.80
cannot build this workspace at all: cargo refuses at resolution, before
compiling anything, because `zvariant_utils` needs the `edition2024` cargo
feature (1.85+) and `time` and the `zbus` 5.x crates require 1.87–1.88. The
number CI enforces is the `1.88.0` pinned in the `MSRV` job, and the header
comment on that job records exactly what fails at each older version.

Two consequences. Do not treat the clippy MSRV lint as your floor — it is
checking against a version the project cannot actually build with. And if you
correct `rust-version` in the root manifest, move the CI pin with it, in the
same change.

## Testing crypto

Read [crypto.md](crypto.md) first if you are touching `openplay-airplay`.

The short version: a round-trip test that runs both sides of a handshake proves
the two sides agree with each other, not that either is correct. Both the
original SRP and the current FairPlay code pass their own round-trip tests while
being unable to talk to any real device. When you add crypto, pin the
*constants* and their properties separately from the protocol flow, and prefer
checking against an independently written implementation over checking against
yourself.

If you cannot verify a constant against a specification or a working
implementation, do not invent one. Leave it unimplemented with a warning.

## Areas where help is useful

The single most valuable contribution right now needs no Rust at all: **run a
cast and report what happened.** Every protocol path in this repository is
connected and none has a confirmed success in the field.

- Running an OpenPlay cast between two real machines and reporting the result.
  It is covered by loopback tests and nothing else
- Testing against real AirPlay and Miracast hardware — no test in this repo can
  substitute for a dongle or an Apple TV
- Confirming AirPlay HAP pairing against real hardware — one `pair_probe` run,
  see [#27](https://github.com/Developer1010x/openplay/issues/27)
- Pairing for OpenPlay, so the receiver's consent prompt is backed by an
  identity rather than by an unauthenticated mDNS record. The
  `PairingChallenge` / `PairingResponse` / `PairingConfirm` messages are already
  defined and unused; see [crypto.md](crypto.md#what-pinning-does-and-does-not-buy)
- Wiring `SenderStateMachine` and `ReceiverStateMachine` into the two session
  loops, which currently enforce ordering by hand
- Audio, of which there is none anywhere
- macOS and Windows screen capture backends. Start with
  `query_primary_display_size()` in `openplay-capture/src/desktop.rs`, whose
  macOS branch is an empty block that silently yields 1920x1080
- AirPlay and Miracast receiver support
- Receiving AirPlay on Linux — see
  [airplay-receiver-design.md](airplay-receiver-design.md)
- Packaging (Flatpak, Homebrew, Winget, AUR)

**Not** useful: porting Apple's FairPlay key tables. That was considered and
declined — see [crypto.md](crypto.md#fairplay--not-fixed). A PR adding them will
be closed, so please do not spend time on it.
