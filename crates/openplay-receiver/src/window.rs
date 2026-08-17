use egui::{Color32, RichText};
use openplay_common::AppConfig;
use tracing::info;

/// Main window for the OpenPlay receiver application.
pub struct ReceiverWindow {
    config: AppConfig,
}

impl ReceiverWindow {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        info!(
            name = %config.display_name,
            port = config.port,
            "Receiver window created"
        );

        Self { config }
    }

    /// The page shown while no sender is connected.
    ///
    // TODO: Phase 1 — once the receiver pipeline is wired up, swap this for the
    // decoded video frames and go fullscreen when a sender connects.
    fn waiting_page(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(&self.config.display_name);
            ui.add_space(8.0);
            ui.label("Waiting for a sender to connect...");
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!("Listening on port {}", self.config.port))
                    .color(Color32::GRAY),
            );
        });
    }
}

impl eframe::App for ReceiverWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| self.waiting_page(ui));
    }
}
