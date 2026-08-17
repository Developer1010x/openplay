use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{info, warn};

use crate::error::OpenPlayError;
use crate::paths::config_dir;
use crate::DEFAULT_PORT;

/// Lowest video bitrate (kbps) that produces a usable stream.
pub const MIN_BITRATE_KBPS: u32 = 100;

/// Upper sanity bound for the configured video bitrate (kbps).
///
/// 100 Mbps is far above anything a screen-cast pipeline needs and almost
/// always indicates a typo (e.g. bytes mistaken for kbps).
pub const MAX_BITRATE_KBPS: u32 = 100_000;

/// Lowest framerate that still produces motion most users would accept.
pub const MIN_FRAMERATE: u32 = 1;

/// Upper sanity bound for the configured framerate.
pub const MAX_FRAMERATE: u32 = 240;

/// Application configuration, loaded from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Human-readable display name for this device.
    pub display_name: String,

    /// Signaling port (receiver only).
    pub port: u16,

    /// Maximum video bitrate in kbps.
    pub max_bitrate_kbps: u32,

    /// Target framerate.
    pub framerate: u32,

    /// Force software encoding (disable HW encoder probing).
    pub force_sw_encode: bool,

    /// Enable AirPlay protocol support.
    pub airplay_enabled: bool,

    /// Enable Miracast protocol support.
    pub miracast_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            display_name: hostname(),
            port: DEFAULT_PORT,
            max_bitrate_kbps: 6000,
            framerate: 30,
            force_sw_encode: false,
            airplay_enabled: true,
            miracast_enabled: true,
        }
    }
}

impl AppConfig {
    /// Loads config from `$XDG_CONFIG_HOME/openplay/config.toml`.
    /// Returns default config if the file doesn't exist.
    pub fn load() -> anyhow::Result<Self> {
        let path = config_dir().join("config.toml");
        Self::load_from(&path)
    }

    /// Loads config from a specific path.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Loads config from the default path and validates it.
    ///
    /// Unlike [`AppConfig::load`], this rejects a config whose values fall
    /// outside the supported ranges instead of silently passing them on to
    /// the pipeline. Returns an [`OpenPlayError::Config`] describing the first
    /// invalid field.
    pub fn load_validated() -> Result<Self, OpenPlayError> {
        let path = config_dir().join("config.toml");
        Self::load_validated_from(&path)
    }

    /// Loads config from a specific path and validates it.
    pub fn load_validated_from(path: &Path) -> Result<Self, OpenPlayError> {
        let config = Self::load_from(path).map_err(|e| OpenPlayError::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Loads config from the default path, writing the defaults out first if no
    /// file exists yet.
    ///
    /// This is what the binaries call at startup: it gives the user a real
    /// `config.toml` to edit after the first run, rather than a path documented
    /// in the README that never materialises.
    pub fn load_or_create() -> Result<Self, OpenPlayError> {
        let path = config_dir().join("config.toml");
        Self::load_or_create_at(&path)
    }

    /// Loads config from a specific path, writing the defaults out first if the
    /// file does not exist.
    ///
    /// A failure to write is logged and ignored — a read-only or unwritable
    /// config directory should not stop the application from starting with
    /// working defaults. A failure to *parse* or *validate* an existing file is
    /// still an error, since that means the user asked for something specific
    /// and we cannot honour it.
    pub fn load_or_create_at(path: &Path) -> Result<Self, OpenPlayError> {
        if !path.exists() {
            let config = Self::default();
            match config.save_to(path) {
                Ok(()) => info!(path = %path.display(), "Wrote default config"),
                Err(e) => warn!(
                    path = %path.display(),
                    error = %e,
                    "Could not write default config, continuing with in-memory defaults"
                ),
            }
            // Defaults are valid by construction, but validate anyway so this
            // path cannot drift away from load_validated_from.
            config.validate()?;
            return Ok(config);
        }

        Self::load_validated_from(path)
    }

    /// Checks that every field holds a value the rest of the system can use.
    ///
    /// This guards against typos in a hand-edited `config.toml` (an empty
    /// display name, a bitrate of zero, an absurd framerate, …) that would
    /// otherwise surface as a confusing failure deep inside the GStreamer
    /// pipeline. Returns the first problem found as an
    /// [`OpenPlayError::Config`].
    pub fn validate(&self) -> Result<(), OpenPlayError> {
        if self.display_name.trim().is_empty() {
            return Err(OpenPlayError::Config(
                "display_name must not be empty".to_string(),
            ));
        }

        // Port 0 means "any port" to the OS, which makes a receiver
        // undiscoverable in practice — require an explicit port.
        if self.port == 0 {
            return Err(OpenPlayError::Config(
                "port must be between 1 and 65535".to_string(),
            ));
        }

        if !(MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS).contains(&self.max_bitrate_kbps) {
            return Err(OpenPlayError::Config(format!(
                "max_bitrate_kbps must be between {MIN_BITRATE_KBPS} and {MAX_BITRATE_KBPS}, got {}",
                self.max_bitrate_kbps
            )));
        }

        if !(MIN_FRAMERATE..=MAX_FRAMERATE).contains(&self.framerate) {
            return Err(OpenPlayError::Config(format!(
                "framerate must be between {MIN_FRAMERATE} and {MAX_FRAMERATE}, got {}",
                self.framerate
            )));
        }

        Ok(())
    }

    /// Saves config to `$XDG_CONFIG_HOME/openplay/config.toml`.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_dir().join("config.toml");
        self.save_to(&path)
    }

    /// Saves config to a specific path.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}

fn hostname() -> String {
    gethostname::gethostname()
        .into_string()
        .unwrap_or_else(|_| "OpenPlay Device".to_string())
}
