use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::{DiscoveryError, DiscoveryEvent};

/// Primary mDNS service type for Miracast over Infrastructure (MICE).
/// Wi-Fi Alliance spec defines this for MICE-capable displays.
const MICE_SERVICE_TYPE: &str = "_display._tcp.local.";

/// Alternative Miracast service type used by many commercial displays
/// (LG CreateBoard, NOVO Pro, ViewSonic, Epson, etc.).
const MIRACAST_SERVICE_TYPE: &str = "_miracast._tcp.local.";

/// WFD (Wi-Fi Display) service type used by some older Miracast implementations.
const WFD_SERVICE_TYPE: &str = "_wfd._tcp.local.";

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

/// Browses the network for Miracast receivers via mDNS.
///
/// Listens on all three known Miracast service types:
/// - `_display._tcp.local.` (Wi-Fi Alliance MICE spec)
/// - `_miracast._tcp.local.` (LG, NOVO, ViewSonic, many education displays)
/// - `_wfd._tcp.local.` (older WFD implementations)
pub struct MiracastBrowser {
    daemon: ServiceDaemon,
}

impl MiracastBrowser {
    /// Creates a new Miracast browser and starts scanning all service types.
    pub fn start() -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            DiscoveryError::Mdns(format!("Failed to create mDNS daemon for Miracast: {e}"))
        })?;

        let (tx, rx) = mpsc::channel(64);

        // Browse all three Miracast service types — different vendors use different types
        for service_type in &[MICE_SERVICE_TYPE, MIRACAST_SERVICE_TYPE, WFD_SERVICE_TYPE] {
            match daemon.browse(service_type) {
                Ok(receiver) => {
                    let tx_clone = tx.clone();
                    let stype = service_type.to_string();
                    let thread_stype = stype.clone();
                    std::thread::Builder::new()
                        .name(format!("miracast-browser-{stype}"))
                        .spawn(move || {
                            run_browser_thread(receiver, tx_clone, &thread_stype);
                        })
                        .map_err(|e| {
                            DiscoveryError::Mdns(format!(
                                "Failed to spawn Miracast browser thread for {stype}: {e}"
                            ))
                        })?;
                    info!(service_type = stype, "Miracast mDNS browser started");
                }
                Err(e) => {
                    debug!(service_type, "Could not browse Miracast service type: {e}");
                }
            }
        }

        info!("Miracast discovery started (MICE + _miracast + _wfd service types)");
        Ok((Self { daemon }, rx))
    }

    /// Stops browsing for Miracast receivers.
    pub fn stop(&self) {
        for stype in &[MICE_SERVICE_TYPE, MIRACAST_SERVICE_TYPE, WFD_SERVICE_TYPE] {
            if let Err(e) = self.daemon.stop_browse(stype) {
                debug!("Failed to stop Miracast mDNS browse for {stype}: {e}");
            }
        }
    }
}

fn run_browser_thread(
    receiver: mdns_sd::Receiver<ServiceEvent>,
    tx: mpsc::Sender<DiscoveryEvent>,
    service_type: &str,
) {
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
                        k == "model" || k == "manufacturer" || k == "deviceinfo" || k == "fn"
                    })
                    .map(|p| p.val_str().to_string())
                    .unwrap_or_default();

                // Use advertised port, fall back to standard WFD RTSP port 7236
                let port = info.get_port();
                let port = if port == 0 { 7236 } else { port };

                let receiver_info = MiracastReceiverInfo {
                    name: info.get_fullname().to_string(),
                    display_name: display_name.clone(),
                    addresses: info.get_addresses().iter().copied().collect(),
                    port,
                    device_info: device_info.clone(),
                };

                info!(
                    name = %receiver_info.display_name,
                    addr = ?receiver_info.addresses,
                    port = receiver_info.port,
                    via = service_type,
                    device = %device_info,
                    "Miracast receiver found"
                );

                if tx
                    .blocking_send(DiscoveryEvent::MiracastReceiverFound(receiver_info))
                    .is_err()
                {
                    debug!("Miracast discovery channel closed");
                    break;
                }
            }
            ServiceEvent::ServiceRemoved(_, name) => {
                info!(name = %name, via = service_type, "Miracast receiver lost");
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
                debug!(service_type, "Miracast mDNS search started");
            }
            _ => {}
        }
    }
}
