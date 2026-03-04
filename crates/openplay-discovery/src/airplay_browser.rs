use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::airplay_record::AirPlayTxtRecord;
use crate::{DiscoveryError, DiscoveryEvent, AIRPLAY_SERVICE_TYPE};

/// Information about a discovered AirPlay receiver.
#[derive(Debug, Clone)]
pub struct AirPlayReceiverInfo {
    /// Service instance name (mDNS fullname).
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// IP addresses.
    pub addresses: Vec<IpAddr>,
    /// AirPlay port.
    pub port: u16,
    /// Device ID (MAC address).
    pub device_id: String,
    /// Feature bitmask string.
    pub features: String,
    /// Device model.
    pub model: String,
}

/// Browses the network for AirPlay receivers via mDNS.
pub struct AirPlayBrowser {
    daemon: ServiceDaemon,
}

impl AirPlayBrowser {
    /// Creates a new AirPlay browser and starts scanning.
    ///
    /// Discovery events are sent to the returned channel.
    pub fn start() -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(|e| {
            DiscoveryError::Mdns(format!("Failed to create mDNS daemon for AirPlay: {e}"))
        })?;

        let receiver = daemon.browse(AIRPLAY_SERVICE_TYPE).map_err(|e| {
            DiscoveryError::Mdns(format!("Failed to browse AirPlay services: {e}"))
        })?;

        let (tx, rx) = mpsc::channel(32);

        std::thread::Builder::new()
            .name("airplay-browser".to_string())
            .spawn(move || {
                for event in receiver {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let props: Vec<(String, String)> = info
                                .get_properties()
                                .iter()
                                .map(|p| (p.key().to_string(), p.val_str().to_string()))
                                .collect();

                            let txt = AirPlayTxtRecord::from_properties(&props);

                            let display_name = info
                                .get_fullname()
                                .split('.')
                                .next()
                                .unwrap_or(info.get_fullname())
                                .to_string();

                            let receiver_info = AirPlayReceiverInfo {
                                name: info.get_fullname().to_string(),
                                display_name,
                                addresses: info.get_addresses().iter().copied().collect(),
                                port: info.get_port(),
                                device_id: txt
                                    .as_ref()
                                    .map(|t| t.device_id.clone())
                                    .unwrap_or_default(),
                                features: txt
                                    .as_ref()
                                    .map(|t| t.features.clone())
                                    .unwrap_or_default(),
                                model: txt
                                    .as_ref()
                                    .map(|t| t.model.clone())
                                    .unwrap_or_default(),
                            };

                            info!(
                                name = %receiver_info.display_name,
                                model = %receiver_info.model,
                                addr = ?receiver_info.addresses,
                                port = receiver_info.port,
                                "AirPlay receiver found"
                            );

                            if tx
                                .blocking_send(DiscoveryEvent::AirPlayReceiverFound(receiver_info))
                                .is_err()
                            {
                                debug!("AirPlay discovery channel closed");
                                break;
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, name) => {
                            info!(name = %name, "AirPlay receiver lost");
                            if tx
                                .blocking_send(DiscoveryEvent::AirPlayReceiverLost {
                                    name: name.to_string(),
                                })
                                .is_err()
                            {
                                debug!("AirPlay discovery channel closed");
                                break;
                            }
                        }
                        ServiceEvent::SearchStarted(_) => {
                            debug!("AirPlay mDNS search started");
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| {
                DiscoveryError::Mdns(format!("Failed to spawn AirPlay browser thread: {e}"))
            })?;

        info!("AirPlay browser started");

        Ok((Self { daemon }, rx))
    }

    /// Stops browsing for AirPlay receivers.
    pub fn stop(&self) {
        if let Err(e) = self.daemon.stop_browse(AIRPLAY_SERVICE_TYPE) {
            error!("Failed to stop AirPlay mDNS browse: {e}");
        }
    }
}
