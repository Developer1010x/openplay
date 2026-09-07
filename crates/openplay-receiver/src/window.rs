use egui::{Color32, RichText};
use openplay_common::AppConfig;
use tracing::info;

use openplay_signaling::ConnectionId;

use crate::net::{NetHandle, SharedFrame, SharedStatus, Status};

/// Main window for the OpenPlay receiver application.
pub struct ReceiverWindow {
    config: AppConfig,
    /// Held so the mDNS registration and signaling server outlive the window,
    /// and so the consent prompt has something to answer through.
    /// `None` when the network side failed to start at all.
    net: Option<NetHandle>,
    status: Option<SharedStatus>,
    frame: Option<SharedFrame>,
    /// The uploaded video texture, and the frame sequence it was built from.
    texture: Option<egui::TextureHandle>,
    texture_sequence: u64,
}

impl ReceiverWindow {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        config: AppConfig,
        net: Option<NetHandle>,
    ) -> Self {
        info!(
            name = %config.display_name,
            port = config.port,
            "Receiver window created"
        );

        let status = net.as_ref().map(|n| n.status());
        let frame = net.as_ref().map(|n| n.frame());

        Self {
            config,
            net,
            status,
            frame,
            texture: None,
            texture_sequence: 0,
        }
    }

    /// Reads the shared status, falling back to a hard failure if the network
    /// side never started or the lock is poisoned.
    fn current_status(&self) -> Status {
        match &self.status {
            Some(shared) => shared
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| Status::Failed {
                    reason: "Status unavailable".to_string(),
                }),
            None => Status::Failed {
                reason: "Discovery and signaling failed to start — see the log".to_string(),
            },
        }
    }

    /// The page shown while no sender is connected.
    fn waiting_page(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(&self.config.display_name);
            ui.add_space(8.0);
            ui.label("Waiting for a sender to connect...");
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!(
                    "Discoverable on this network · port {}",
                    self.config.port
                ))
                .color(Color32::GRAY),
            );
        });
    }

    /// The approval prompt.
    ///
    /// This is the only thing standing between a stranger on the network and
    /// this screen: there is no pairing and no authentication behind it, so the
    /// prompt names the sender and defaults to nothing happening. It must stay
    /// an explicit, positive action — never a timeout that accepts.
    fn consent_page(&mut self, ui: &mut egui::Ui, sender_name: &str, connection: ConnectionId) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.2);
            ui.heading("Allow this device to cast?");
            ui.add_space(12.0);
            ui.label(
                RichText::new(sender_name)
                    .size(22.0)
                    .color(Color32::from_rgb(120, 180, 255)),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "OpenPlay cannot verify who this is. Only accept a device you recognise.",
                )
                .color(Color32::GRAY),
            );
            ui.add_space(24.0);

            ui.horizontal(|ui| {
                // Centre the pair of buttons by padding to the left.
                let button_block = 260.0;
                let pad = (ui.available_width() - button_block).max(0.0) / 2.0;
                ui.add_space(pad);

                if ui
                    .add_sized(
                        [120.0, 36.0],
                        egui::Button::new(RichText::new("Allow").size(16.0)),
                    )
                    .clicked()
                {
                    self.decide(connection, true);
                }
                ui.add_space(20.0);
                if ui
                    .add_sized(
                        [120.0, 36.0],
                        egui::Button::new(RichText::new("Deny").size(16.0)),
                    )
                    .clicked()
                {
                    self.decide(connection, false);
                }
            });
        });
    }

    fn decide(&self, connection: ConnectionId, accept: bool) {
        if let Some(net) = &self.net {
            net.decide(connection, accept);
        }
    }

    /// A short status line shown while the media session is being set up.
    fn negotiating_page(&self, ui: &mut egui::Ui, sender_name: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(&self.config.display_name);
            ui.add_space(8.0);
            ui.add(egui::Spinner::new());
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("Connecting to {sender_name}..."))
                    .color(Color32::from_rgb(80, 200, 120)),
            );
        });
    }

    fn failed_page(&self, ui: &mut egui::Ui, reason: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading(&self.config.display_name);
            ui.add_space(8.0);
            ui.label(RichText::new(reason).color(Color32::from_rgb(230, 120, 100)));
        });
    }

    /// Uploads the newest decoded frame, if there is one we have not shown yet.
    ///
    /// Returns the texture to paint. Re-uploading only on a sequence change
    /// keeps a paused or slow stream from re-sending the same pixels to the GPU
    /// at the repaint rate.
    fn refresh_texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        let shared = self.frame.as_ref()?;
        let (image, sequence) = {
            let slot = shared.lock().ok()?;
            if slot.sequence == self.texture_sequence {
                return self.texture.as_ref();
            }
            let frame = slot.frame.as_ref()?;
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            (image, slot.sequence)
        };

        match &mut self.texture {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("openplay-video", image, egui::TextureOptions::LINEAR));
            }
        }
        self.texture_sequence = sequence;
        self.texture.as_ref()
    }

    /// Paints the incoming video, letterboxed to preserve its aspect ratio.
    fn video_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, sender_name: &str) {
        let available = ui.available_size();
        let Some(texture) = self.refresh_texture(ctx) else {
            // Connected, but the first frame has not arrived yet.
            ui.vertical_centered(|ui| {
                ui.add_space(available.y * 0.4);
                ui.add(egui::Spinner::new());
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("Receiving from {sender_name}...")).color(Color32::GRAY),
                );
            });
            return;
        };

        let size = texture.size_vec2();
        let scale = (available.x / size.x).min(available.y / size.y);
        let scaled = size * scale;

        ui.centered_and_justified(|ui| {
            ui.add(egui::Image::new(texture).fit_to_exact_size(scaled));
        });
    }
}

impl eframe::App for ReceiverWindow {
    /// Paint the letterbox bars black rather than the theme background, so a
    /// non-16:9 stream on a TV does not glow grey around the edges.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let status = self.current_status();

        let streaming = matches!(status, Status::Streaming { .. });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| match &status {
                Status::Waiting => self.waiting_page(ui),
                Status::PendingConsent {
                    sender_name,
                    connection,
                } => {
                    let sender_name = sender_name.clone();
                    self.consent_page(ui, &sender_name, *connection);
                }
                Status::Negotiating { sender_name } => self.negotiating_page(ui, sender_name),
                Status::Streaming { sender_name } => {
                    let sender_name = sender_name.clone();
                    self.video_page(ui, ctx, &sender_name);
                }
                Status::Failed { reason } => self.failed_page(ui, reason),
            });

        // The status and frame slots are written from signaling and GStreamer
        // threads, neither of which can wake the UI. Repaint on a timer —
        // quickly while video is arriving, lazily while idle, so a waiting
        // receiver does not spin a core on a TV box.
        let interval = if streaming {
            std::time::Duration::from_millis(16)
        } else {
            std::time::Duration::from_millis(250)
        };
        ctx.request_repaint_after(interval);
    }
}
