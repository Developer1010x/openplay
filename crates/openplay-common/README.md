# openplay-common

Shared foundation for all OpenPlay crates. Provides configuration, logging, XDG path resolution, and the top-level error type.

## What it contains

**`AppConfig`** — the application configuration struct, serialized to `config.toml`. Covers display name, signaling port, max bitrate, framerate, hardware encode toggle, and per-protocol enable flags. Loaded with `AppConfig::load()` (from XDG config dir) or `AppConfig::load_from(path)`. Saved with `save()` / `save_to(path)`.

**`OpenPlayError`** — the shared error type used across crate boundaries. Variants map to subsystems: `Config`, `Io`, `Certificate`, `Discovery`, `Signaling`, `Pipeline`, `Capture`, `Protocol`, `Timeout`.

**`init_logging()`** — initializes `tracing-subscriber` with `RUST_LOG` env support. Call once at the start of `main()`.

**Path helpers** — `config_dir()`, `data_dir()`, `ensure_dirs()`. All paths follow XDG Base Directory spec on Linux, standard platform conventions on macOS and Windows. Both binaries call `ensure_dirs()` at startup.

**Constants** — `DEFAULT_PORT` (7290), `PROTOCOL_VERSION` (1), `MDNS_SERVICE_TYPE`, `AIRPLAY_MDNS_SERVICE_TYPE`, `AIRPLAY_NTP_PORT`, `AIRPLAY_DEFAULT_PORT`.

## Usage

```rust
use openplay_common::{AppConfig, init_logging, ensure_dirs};

init_logging();
let config = AppConfig::load()?;
ensure_dirs()?;
```

## Tests

```bash
cargo test -p openplay-common
```
