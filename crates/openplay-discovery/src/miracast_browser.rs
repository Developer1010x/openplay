use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::{DiscoveryError, DiscoveryEvent};

/// mDNS service type for Miracast over Infrastructure (MICE).
/// Wi-Fi Alliance spec defines this for MICE-capable displays.
const MICE_SERVICE_TYPE: &str = "_display._tcp.local.";

/// Information about a discovered Miracast receiver.
#[derive(Debug, Clone)]
pub struct MiracastReceiverInfo {
    /// Service instance name (mDNS fullname).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// IP addresses.
    pub addresses: Vec<IpAddr>,
    /// Miracast RTSP port (default 7236).
    pub port: u16,
    /// Device manufacturer/model from TXT records (if available).
    pub device_info: String,
}

/// Browses the network for Miracast MICE receivers via mDNS.
pub struct MiracastBrowser {
    daemon: ServiceDaemon,
}

impl MiracastBrowser {
    /// Creates a new Miracast MICE browser and starts scanning.
    ///
    /// Discovers displays advertising `_display._tcp.local.` which indicates
    /// Miracast over Infrastructure (MICE) support — same as Windows auto-detect.
    pub fn start() -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            DiscoveryError::Mdns(format!("Failed to create mDNS daemon for Miracast: {e}"))
        })?;

        let receiver = daemon.browse(MICE_SERVICE_TYPE).map_err(|e| {
            DiscoveryError::Mdns(format!("Failed to browse Miracast MICE services: {e}"))
        })?;

        let (tx, rx) = mpsc::channel(32);

        std::thread::Builder::new()
            .name("miracast-browser".to_string())
            .spawn(move || {
                for event in receiver {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let display_name = info
                                .get_fullname()
                                .split('.')
                                .next()
                                .unwrap_or(info.get_fullname())
                                .to_string();

                            // Extract device info from TXT records if available
                            let device_info = info
                                .get_properties()
                                .iter()
                                .find(|p| {
                                    let k = p.key().to_lowercase();
                                    k == "model" || k == "manufacturer" || k == "deviceinfo"
                                })
                                .map(|p| p.val_str().to_string())
                                .unwrap_or_default();

                            // Use advertised port, fall back to standard WFD RTSP port
                            let port = info.get_port();
                            let port = if port == 0 { 7236 } else { port };

                            let receiver_info = MiracastReceiverInfo {
                                name: info.get_fullname().to_string(),
                                display_name,
                                addresses: info.get_addresses().iter().copied().collect(),
                                port,
                                device_info,
                            };

                            info!(
                                name = %receiver_info.display_name,
                                addr = ?receiver_info.addresses,
                                port = receiver_info.port,
                                "Miracast MICE receiver found"
                            );

                            if tx
                                .blocking_send(DiscoveryEvent::MiracastReceiverFound(
                                    receiver_info,
                                ))
                                .is_err()
                            {
                                debug!("Miracast discovery channel closed");
                                break;
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, name) => {
                            info!(name = %name, "Miracast MICE receiver lost");
                            if tx
                                .blocking_send(DiscoveryEvent::MiracastReceiverLost {
                                    name: name.to_string(),
                                })
                                .is_err()
                            {
                                debug!("Miracast discovery channel closed");
                                break;
                            }
                        }
                        ServiceEvent::SearchStarted(_) => {
                            debug!("Miracast MICE mDNS search started");
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| {
                DiscoveryError::Mdns(format!("Failed to spawn Miracast browser thread: {e}"))
            })?;

        info!("Miracast MICE browser started");

        Ok((Self { daemon }, rx))
    }

    /// Stops browsing for Miracast receivers.
    pub fn stop(&self) {
        if let Err(e) = self.daemon.stop_browse(MICE_SERVICE_TYPE) {
            tracing::error!("Failed to stop Miracast MICE mDNS browse: {e}");
        }
    }
}
