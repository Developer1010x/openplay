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

## CI jobs

| Job | Runs on | Covers |
|---|---|---|
| `Check & Lint` | ubuntu-24.04 | `fmt --check`, then `clippy -D warnings` |
| `Test (ubuntu-24.04)` | ubuntu-24.04 | `cargo test --all` |
| `Cross-platform check (macos-14 / windows-2022)` | macos-14, windows-2022 | `cargo check --all-targets` on the eight crates that need no system packages |
| `Build Release` | ubuntu-24.04 | `cargo build --release`, uploads binaries |

The cross-platform job covers `openplay-common`, `-protocol`, `-crypto`,
`-capture`, `-discovery`, `-signaling`, `-airplay` and `-miracast` — everything
that builds without GStreamer or PipeWire.

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

**The MSRV is real.** Every crate inherits `rust-version` from the workspace, so
clippy fails the build on anything stabilised after 1.80. Adding the inheritance
immediately caught a use of `is_multiple_of`, stable only since 1.87.

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

- FairPlay key derivation — see [crypto.md](crypto.md#fairplay--not-fixed)
- Wiring the OpenPlay/WebRTC path to the two binaries. The receiver is currently
  a window with a single dependency, so this is close to greenfield
- Receiving AirPlay on Linux — see
  [airplay-receiver-design.md](airplay-receiver-design.md)
- macOS and Windows screen capture backends
- AirPlay and Miracast receiver support
- Testing against real AirPlay and Miracast hardware — genuinely valuable, since
  no test in this repo can substitute
- Packaging (Flatpak, Homebrew, Winget, AUR)
