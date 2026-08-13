use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::net::IpAddr;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::{record::TxtRecord, DiscoveryError, DiscoveryEvent, SERVICE_TYPE};

/// Information about a discovered receiver on the network.
#[derive(Debug, Clone)]
pub struct ReceiverInfo {
    /// Service instance name.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// IP addresses of the receiver.
    pub addresses: Vec<IpAddr>,
    /// Signaling port.
    pub port: u16,
    /// Certificate fingerprint.
    pub fingerprint: String,
    /// Supported video codecs.
    pub video_codecs: Vec<String>,
    /// Maximum resolution.
    pub resolution: String,
    /// Maximum framerate.
    pub max_fps: u32,
}

/// Browses the network for OpenPlay receivers via mDNS.
pub struct ReceiverBrowser {
    daemon: ServiceDaemon,
}

impl ReceiverBrowser {
    /// Creates a new browser and starts scanning for receivers.
    ///
    /// Discovery events are sent to the returned channel.
    pub fn start() -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| DiscoveryError::Mdns(format!("Failed to create mDNS daemon: {e}")))?;

        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| DiscoveryError::Mdns(format!("Failed to browse: {e}")))?;

        let (tx, rx) = mpsc::channel(32);

        // Spawn a task to process mDNS events and forward them
        std::thread::Builder::new()
            .name("mdns-browser".to_string())
            .spawn(move || {
                for event in receiver {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let props: Vec<(String, String)> = info
                                .get_properties()
                                .iter()
                                .map(|p| (p.key().to_string(), p.val_str().to_string()))
                                .collect();

                            let txt = TxtRecord::from_properties(&props);
                            let display_name = txt
                                .as_ref()
                                .map(|t| t.display_name.clone())
                                .unwrap_or_else(|| info.get_fullname().to_string());

                            let receiver_info = ReceiverInfo {
                                name: info.get_fullname().to_string(),
                                display_name,
                                addresses: info.get_addresses().iter().copied().collect(),
                                port: info.get_port(),
                                fingerprint: txt
                                    .as_ref()
                                    .map(|t| t.fingerprint.clone())
                                    .unwrap_or_default(),
                                video_codecs: txt
                                    .as_ref()
                                    .map(|t| {
                                        t.video_codecs.split(',').map(|s| s.to_string()).collect()
                                    })
                                    .unwrap_or_else(|| vec!["h264".to_string()]),
                                resolution: txt
                                    .as_ref()
                                    .map(|t| t.resolution.clone())
                                    .unwrap_or_else(|| "1920x1080".to_string()),
                                max_fps: txt.as_ref().map(|t| t.max_fps).unwrap_or(30),
                            };

                            info!(
                                name = %receiver_info.display_name,
                                addr = ?receiver_info.addresses,
                                port = receiver_info.port,
                                "Receiver found"
                            );

                            if tx
                                .blocking_send(DiscoveryEvent::ReceiverFound(receiver_info))
                                .is_err()
                            {
                                debug!("Discovery event channel closed");
                                break;
                            }
                        }
                        ServiceEvent::ServiceRemoved(_, name) => {
                            info!(name = %name, "Receiver lost");
                            if tx
                                .blocking_send(DiscoveryEvent::ReceiverLost {
                                    name: name.to_string(),
                                })
                                .is_err()
                            {
                                debug!("Discovery event channel closed");
                                break;
                            }
                        }
                        ServiceEvent::SearchStarted(_) => {
                            debug!("mDNS search started");
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|e| DiscoveryError::Mdns(format!("Failed to spawn browser thread: {e}")))?;

        info!("Receiver browser started");

        Ok((Self { daemon }, rx))
    }

    /// Stops browsing for receivers.
    pub fn stop(&self) {
        if let Err(e) = self.daemon.stop_browse(SERVICE_TYPE) {
            error!("Failed to stop mDNS browse: {e}");
        }
    }
}
