mod config;
mod error;
mod logging;
mod paths;

pub use config::{AppConfig, MAX_BITRATE_KBPS, MAX_FRAMERATE, MIN_BITRATE_KBPS, MIN_FRAMERATE};
pub use error::OpenPlayError;
pub use logging::init_logging;
pub use paths::{config_dir, data_dir, ensure_dirs};

/// Default signaling port for OpenPlay receivers.
pub const DEFAULT_PORT: u16 = 7290;

/// Protocol version for signaling messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// mDNS service type.
pub const MDNS_SERVICE_TYPE: &str = "_openplay._tcp.local.";

/// AirPlay mDNS service type.
pub const AIRPLAY_MDNS_SERVICE_TYPE: &str = "_airplay._tcp.local.";

/// AirPlay NTP server port.
pub const AIRPLAY_NTP_PORT: u16 = 7010;

/// AirPlay default TCP port for HTTP + mirror stream.
pub const AIRPLAY_DEFAULT_PORT: u16 = 7000;
