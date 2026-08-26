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

    /// The page shown while the receiver cannot yet be reached.
    ///
    /// This deliberately does not say "waiting for a sender" or "listening on
    /// port N". Neither was ever true: the receiver opens no socket and
    /// advertises no mDNS service, so a sender cannot discover it or connect to
    /// it. Claiming otherwise sent at least one person debugging their network
    /// for a feature that does not exist.
    ///
    // TODO: Phase 1 — once the receiver pipeline is wired up, swap this for the
    // decoded video frames and go fullscreen when a sender connects.
    fn waiting_page(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(&self.config.display_name);
            ui.add_space(8.0);
            ui.label("Not reachable yet — this receiver is a placeholder.");
            ui.add_space(16.0);
            ui.label(
                RichText::new(
                    "It does not advertise itself over mDNS and accepts no\n\
                     connections, so senders cannot discover or reach it.",
                )
                .color(Color32::GRAY),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new("Tracking: Developer1010x/openplay#11")
                    .small()
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
