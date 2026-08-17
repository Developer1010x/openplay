# OpenPlay documentation

## Using it

- **[install.md](install.md)** — prerequisites, building, running, and what
  works on which platform
- **[configuration.md](configuration.md)** — every `config.toml` key, its valid
  range, CLI overrides, and logging
- **[troubleshooting.md](troubleshooting.md)** — capture, encoder, discovery and
  per-protocol failures, starting with the symptoms that are unimplemented
  features rather than bugs

## Working on it

- **[architecture.md](architecture.md)** — crate graph, how the three casting
  paths fit together, and the conventions that are easy to break by accident
- **[protocols.md](protocols.md)** — Miracast RTSP M1–M7, the AirPlay handshake,
  and the OpenPlay/WebRTC message set and state machines
- **[crypto.md](crypto.md)** — what is fixed, what is not, and why the tests are
  shaped the way they are. Read this before touching `openplay-airplay`
- **[contributing.md](contributing.md)** — the commands CI runs, platform
  gating, and how to test crypto without fooling yourself
- **[packaging.md](packaging.md)** — what lives in `data/` and `flatpak/`, and
  the D-Bus and polkit setup Wi-Fi Direct needs

## Planned work

- **[airplay-receiver-design.md](airplay-receiver-design.md)** — design for
  receiving AirPlay on Linux (iPhone or Mac casting *to* this machine). Design
  only, no code, blocked on the FairPlay decision

## Start here

Depending on what you are doing:

| If you want to… | Read |
|---|---|
| Cast something | [install.md](install.md) |
| Work out why it will not cast | [troubleshooting.md](troubleshooting.md) |
| Understand the codebase | [architecture.md](architecture.md) |
| Implement a protocol feature | [protocols.md](protocols.md) |
| Touch anything cryptographic | [crypto.md](crypto.md) |
| Package it, or fix Wi-Fi Direct permissions | [packaging.md](packaging.md) |
| Receive AirPlay on Linux | [airplay-receiver-design.md](airplay-receiver-design.md) |
| Send a pull request | [contributing.md](contributing.md) |

The [README](../README.md) Status section is the authority on what currently
works. These documents describe the design, including parts that are built but
not yet connected — where that is true, they say so explicitly.
