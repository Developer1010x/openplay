use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{error, info};

use crate::{record::TxtRecord, DiscoveryError, SERVICE_TYPE};

/// Advertises this receiver on the local network via mDNS.
pub struct ReceiverAdvertiser {
    daemon: ServiceDaemon,
    instance_name: String,
}

impl ReceiverAdvertiser {
    /// Creates a new advertiser and registers the service.
    pub fn new(txt_record: &TxtRecord) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| DiscoveryError::Mdns(format!("Failed to create mDNS daemon: {e}")))?;

        let instance_name = format!("{}.{}", txt_record.display_name, SERVICE_TYPE);
        let host_name = format!(
            "{}.local.",
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "openplay".to_string())
        );

        let properties = txt_record.to_properties();
        let props: Vec<(&str, &str)> = properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &txt_record.display_name,
            &host_name,
            "",
            txt_record.port,
            &props[..],
        )
        .map_err(|e| DiscoveryError::Registration(format!("Failed to create service info: {e}")))?;

        daemon.register(service).map_err(|e| {
            DiscoveryError::Registration(format!("Failed to register service: {e}"))
        })?;

        info!(
            name = %txt_record.display_name,
            port = txt_record.port,
            "mDNS service registered"
        );

        Ok(Self {
            daemon,
            instance_name,
        })
    }

    /// Unregisters the mDNS service.
    pub fn stop(&self) {
        if let Err(e) = self.daemon.unregister(&self.instance_name) {
            error!("Failed to unregister mDNS service: {e}");
        } else {
            info!("mDNS service unregistered");
        }
    }
}

impl Drop for ReceiverAdvertiser {
    fn drop(&mut self) {
        self.stop();
    }
}
