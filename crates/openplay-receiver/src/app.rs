use openplay_common::AppConfig;

use crate::window::ReceiverWindow;

/// Runs the receiver application.
pub fn run(config: AppConfig) -> anyhow::Result<()> {
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
        Box::new(|cc| Ok(Box::new(ReceiverWindow::new(cc, config)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}
