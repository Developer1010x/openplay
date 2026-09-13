use openplay_common::AppConfig;
use tracing::error;

use crate::net;
use crate::window::ReceiverWindow;

/// Runs the receiver application.
pub fn run(config: AppConfig) -> anyhow::Result<()> {
    // Start discovery and signaling before the window, so the mDNS service is
    // registered by the time the "waiting" page appears. A failure here is
    // reported in the window rather than aborting: a receiver that cannot
    // advertise is still worth showing, if only to say why.
    let net = match net::start(&config) {
        Ok(handle) => Some(handle),
        Err(e) => {
            error!("Failed to start discovery and signaling: {e:#}");
            None
        }
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpenPlay Receiver")
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };

    eframe::run_native(
        "OpenPlay Receiver",
        native_options,
        Box::new(|cc| Ok(Box::new(ReceiverWindow::new(cc, config, net)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}
