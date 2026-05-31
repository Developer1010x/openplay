# openplay-common

Shared foundation for all OpenPlay crates. Provides configuration, logging, XDG path resolution, and the top-level error type.

## What it contains

**`AppConfig`** — the application configuration struct, serialized to `config.toml`. Covers display name, signaling port, max bitrate, framerate, hardware encode toggle, and per-protocol enable flags. Loaded with `AppConfig::load()` (from XDG config dir) or `AppConfig::load_from(path)`. Saved with `save()` / `save_to(path)`.

`AppConfig::validate()` checks that the loaded values fall within supported ranges (non-empty display name, non-zero port, bitrate within `MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS`, framerate within `MIN_FRAMERATE..=MAX_FRAMERATE`) and returns the first problem as an `OpenPlayError::Config`. Use `AppConfig::load_validated()` / `load_validated_from(path)` to load and validate in one step, catching typos in a hand-edited `config.toml` before they surface as confusing failures deep inside the GStreamer pipeline.

**`OpenPlayError`** — the shared error type used across crate boundaries. Variants map to subsystems: `Config`, `Io`, `Certificate`, `Discovery`, `Signaling`, `Pipeline`, `Capture`, `Protocol`, `Timeout`.

**`init_logging()`** — initializes `tracing-subscriber` with `RUST_LOG` env support. Call once at the start of `main()`.

**Path helpers** — `config_dir()`, `data_dir()`, `ensure_dirs()`. All paths follow XDG Base Directory spec on Linux, standard platform conventions on macOS and Windows. Both binaries call `ensure_dirs()` at startup.

**Constants** — `DEFAULT_PORT` (7290), `PROTOCOL_VERSION` (1), `MDNS_SERVICE_TYPE`, `AIRPLAY_MDNS_SERVICE_TYPE`, `AIRPLAY_NTP_PORT`, `AIRPLAY_DEFAULT_PORT`, and the config validation bounds `MIN_BITRATE_KBPS`, `MAX_BITRATE_KBPS`, `MIN_FRAMERATE`, `MAX_FRAMERATE`.

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
