use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::paths::config_dir;
use crate::DEFAULT_PORT;

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
