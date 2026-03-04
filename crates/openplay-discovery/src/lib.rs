mod advertiser;
pub mod airplay_browser;
pub mod airplay_record;
mod browser;
pub mod miracast_browser;
mod record;

pub use advertiser::ReceiverAdvertiser;
pub use airplay_browser::{AirPlayBrowser, AirPlayReceiverInfo};
pub use airplay_record::AirPlayTxtRecord;
pub use browser::{ReceiverBrowser, ReceiverInfo};
pub use miracast_browser::{MiracastBrowser, MiracastReceiverInfo};
pub use record::TxtRecord;

use thiserror::Error;

/// mDNS service type for OpenPlay receivers.
pub const SERVICE_TYPE: &str = "_openplay._tcp.local.";

/// mDNS service type for AirPlay receivers.
pub const AIRPLAY_SERVICE_TYPE: &str = "_airplay._tcp.local.";

/// Default signaling port.
pub const DEFAULT_PORT: u16 = 7290;

#[derive(Error, Debug)]
pub enum DiscoveryError {
    #[error("mDNS error: {0}")]
    Mdns(String),

    #[error("Service registration failed: {0}")]
    Registration(String),
}

/// Events emitted during receiver discovery.
#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    /// An OpenPlay receiver was found on the network.
    ReceiverFound(ReceiverInfo),
    /// An OpenPlay receiver is no longer available.
    ReceiverLost { name: String },
    /// An AirPlay receiver was found on the network.
    AirPlayReceiverFound(AirPlayReceiverInfo),
    /// An AirPlay receiver is no longer available.
    AirPlayReceiverLost { name: String },
    /// A Miracast MICE receiver was found on the network.
    MiracastReceiverFound(MiracastReceiverInfo),
    /// A Miracast MICE receiver is no longer available.
    MiracastReceiverLost { name: String },
}
