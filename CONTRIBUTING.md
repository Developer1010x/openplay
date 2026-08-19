# Contributing to OpenPlay

Thanks for looking. OpenPlay is a screen-casting stack in Rust — AirPlay,
Miracast and a native WebRTC protocol — and it is early enough that there is
plenty of well-defined work available.

**New here? Start with [good first issues](https://github.com/Developer1010x/openplay/labels/good%20first%20issue).**

## In 60 seconds

```bash
git clone https://github.com/Developer1010x/openplay.git
cd openplay
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  libgstreamer-plugins-bad1.0-dev gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
  gstreamer1.0-pipewire libpipewire-0.3-dev        # Ubuntu/Debian
cargo build
cargo test --all
```

Other platforms and the full dependency list: [docs/install.md](docs/install.md).

## Before you push

CI runs these, and a formatting failure hides every lint finding behind it:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Read this first

Two documents will save you the most time:

- **[docs/architecture.md](docs/architecture.md)** — how the eleven crates fit
  together, and the conventions that are easy to break by accident.
- **[docs/contributing.md](docs/contributing.md)** — the full guide: CI job
  layout, platform gating, and how to test cryptography without fooling
  yourself.

If you are touching `openplay-airplay`, read
**[docs/crypto.md](docs/crypto.md)** before you start. It explains which parts
are known-broken and why, which will stop you debugging the wrong layer.

## What this project needs

Honest status lives in the [README](README.md#status). The short version is that
Miracast sending works, AirPlay sending is partly working, and the native
OpenPlay/WebRTC path is built as libraries but not connected to either binary.

Areas where help goes furthest:

| Area | Why it matters | Difficulty |
|---|---|---|
| Testing against real hardware | No test in this repo can substitute for an Apple TV or a Miracast dongle. Even a failure report is valuable | Easy |
| Documentation fixes | The docs are extensively cross-checked against source; keeping them true as code changes is ongoing | Easy |
| Wiring the OpenPlay/WebRTC path | The libraries exist and are unreferenced; the receiver is close to greenfield | Medium |
| Audio support | There is none anywhere in the workspace, though the protocol layer advertises it | Medium |
| macOS/Windows capture | Compiles, never verified | Medium |
| [AirPlay receiver](docs/airplay-receiver-design.md) | Let iPhones cast *to* Linux. Designed, blocked on a licensing decision | Hard |

## Ground rules

**Open an issue before a large change**, so the approach can be agreed before
you write code. Small fixes can go straight to a PR.

**Say what you verified.** "Ran `cargo test --all`, and checked the Windows path
with `cargo check --target x86_64-pc-windows-msvc`" is worth more than a
confident description. If you could not test something, say that instead — an
honest gap is fine, an unstated one is not.

**Do not invent constants.** This codebase has already shipped fabricated
cryptographic values that looked plausible and passed their own tests. If you
cannot verify a value against a specification or a working implementation, leave
it unimplemented with a warning rather than guessing. See
[docs/crypto.md](docs/crypto.md).

**No CLA, no copyright assignment.** Contributions are under
[GPL-3.0-or-later](LICENSE), same as the project.

Questions are welcome as issues. "I tried to do X and got lost at Y" is a
legitimate issue and usually means the documentation is at fault.
