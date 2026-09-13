use std::net::{IpAddr, SocketAddr};

use openplay_discovery::{AirPlayReceiverInfo, ReceiverInfo};

/// Protocol type for a discovered receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    OpenPlay,
    AirPlay,
    Miracast,
}

impl Protocol {
    /// Short label for UI display.
    pub fn label(&self) -> &'static str {
        match self {
            Protocol::OpenPlay => "OpenPlay",
            Protocol::AirPlay => "AirPlay",
            Protocol::Miracast => "Miracast",
        }
    }
}

/// A discovered receiver from any protocol, with a common interface.
#[derive(Debug, Clone)]
pub enum DiscoveredReceiver {
    OpenPlay(ReceiverInfo),
    AirPlay(AirPlayReceiverInfo),
    Miracast(MiracastReceiver),
}

/// Miracast connection mode.
#[derive(Debug, Clone)]
pub enum MiracastMode {
    /// MICE: Miracast over Infrastructure (existing Wi-Fi network).
    Infrastructure { addr: IpAddr, port: u16 },
    /// Wi-Fi Direct P2P: connect via wpa_supplicant P2P.
    WifiDirect { device_address: String },
}

/// A Miracast receiver (manually entered or discovered via Wi-Fi Direct).
#[derive(Debug, Clone)]
pub struct MiracastReceiver {
    pub display_name: String,
    pub mode: MiracastMode,
}

impl DiscoveredReceiver {
    /// Human-readable display name.
    pub fn display_name(&self) -> &str {
        match self {
            DiscoveredReceiver::OpenPlay(r) => &r.display_name,
            DiscoveredReceiver::AirPlay(r) => &r.display_name,
            DiscoveredReceiver::Miracast(r) => &r.display_name,
        }
    }

    /// Protocol type.
    pub fn protocol(&self) -> Protocol {
        match self {
            DiscoveredReceiver::OpenPlay(_) => Protocol::OpenPlay,
            DiscoveredReceiver::AirPlay(_) => Protocol::AirPlay,
            DiscoveredReceiver::Miracast(_) => Protocol::Miracast,
        }
    }

    /// The receiver's TLS certificate fingerprint, from its mDNS `fp` TXT key.
    ///
    /// Only OpenPlay receivers publish one. It is required to connect: the
    /// receiver's certificate is self-signed, so the fingerprint is the only
    /// thing that distinguishes it from any other host that answers.
    pub fn fingerprint(&self) -> Option<&str> {
        match self {
            DiscoveredReceiver::OpenPlay(r) => Some(&r.fingerprint),
            DiscoveredReceiver::AirPlay(_) | DiscoveredReceiver::Miracast(_) => None,
        }
    }

    /// Primary address for connection.
    pub fn addr(&self) -> Option<SocketAddr> {
        match self {
            DiscoveredReceiver::OpenPlay(r) => {
                r.addresses.first().map(|ip| SocketAddr::new(*ip, r.port))
            }
            DiscoveredReceiver::AirPlay(r) => {
                r.addresses.first().map(|ip| SocketAddr::new(*ip, r.port))
            }
            DiscoveredReceiver::Miracast(r) => match &r.mode {
                MiracastMode::Infrastructure { addr, port } => Some(SocketAddr::new(*addr, *port)),
                MiracastMode::WifiDirect { .. } => None, // No IP until P2P group formed
            },
        }
    }

    /// Every address the receiver advertised, most-connectable first.
    ///
    /// `addr()` returns only the first of these, which is not enough for
    /// OpenPlay: a receiver on a machine running Docker advertises its bridge
    /// addresses alongside its real one, and `172.17.0.1` sorts ahead of
    /// `192.168.1.10` because `sort_by_connectability` ranks all routable IPv4
    /// alike and then breaks ties on the numeric value. Dialling only the first
    /// address reaches the *sender's own* Docker bridge and never the receiver.
    ///
    /// Ranking cannot fix this on its own — the sender's `docker0` really is
    /// `172.17.0.1/16`, so a same-subnet test also prefers the wrong address.
    /// The caller therefore tries these in order, and the pinned certificate
    /// decides which one was actually the receiver.
    pub fn addrs(&self) -> Vec<SocketAddr> {
        match self {
            DiscoveredReceiver::OpenPlay(r) => r
                .addresses
                .iter()
                .map(|ip| SocketAddr::new(*ip, r.port))
                .collect(),
            DiscoveredReceiver::AirPlay(r) => r
                .addresses
                .iter()
                .map(|ip| SocketAddr::new(*ip, r.port))
                .collect(),
            DiscoveredReceiver::Miracast(_) => self.addr().into_iter().collect(),
        }
    }

    /// Wi-Fi Direct device address (for P2P Miracast).
    ///
    /// Linux-only: its sole caller in `app.rs` starts a Wi-Fi Direct P2P cast,
    /// which is gated to Linux because `wifi_direct.rs` drives wpa_supplicant
    /// over D-Bus. Without this gate the method is dead code on macOS and
    /// Windows, and `clippy -D warnings` fails there.
    #[cfg(target_os = "linux")]
    pub fn wifi_direct_address(&self) -> Option<&str> {
        match self {
            DiscoveredReceiver::Miracast(r) => match &r.mode {
                MiracastMode::WifiDirect { device_address } => Some(device_address),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether this is a Wi-Fi Direct Miracast receiver.
    pub fn is_wifi_direct(&self) -> bool {
        matches!(self, DiscoveredReceiver::Miracast(r) if matches!(&r.mode, MiracastMode::WifiDirect { .. }))
    }

    /// Unique key for deduplication in lists.
    pub fn key(&self) -> String {
        match self {
            DiscoveredReceiver::OpenPlay(r) => r.name.clone(),
            DiscoveredReceiver::AirPlay(r) => r.name.clone(),
            DiscoveredReceiver::Miracast(r) => match &r.mode {
                MiracastMode::Infrastructure { addr, .. } => format!("miracast-{addr}"),
                MiracastMode::WifiDirect { device_address } => {
                    format!("miracast-p2p-{device_address}")
                }
            },
        }
    }
}
