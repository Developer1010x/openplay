use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use openplay_common::AppConfig;
use tracing::info;

/// Main window for the OpenPlay receiver application.
pub struct ReceiverWindow {
    window: adw::ApplicationWindow,
}

impl ReceiverWindow {
    pub fn new(app: &adw::Application, config: &AppConfig) -> Self {
        let header = adw::HeaderBar::new();

        // Status page shown when waiting for a connection
        let status_page = adw::StatusPage::builder()
            .title(&config.display_name)
            .description("Waiting for a sender to connect...")
            .icon_name("network-wireless-symbolic")
            .build();

        // Connection info
        let info_label = gtk::Label::builder()
            .label(format!("Listening on port {}", config.port))
            .css_classes(vec!["dim-label"])
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        content.append(&status_page);
        content.append(&info_label);

        let toolbar_view = adw::ToolbarView::builder().build();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&content));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("OpenPlay Receiver")
            .default_width(800)
            .default_height(600)
            .content(&toolbar_view)
            .build();

        info!(
            name = %config.display_name,
            port = config.port,
            "Receiver window created"
        );

        Self { window }
    }

    /// Switches from the waiting page to fullscreen video display.
    pub fn show_video(&self, _paintable: &impl gtk::prelude::IsA<gtk::gdk::Paintable>) {
        // TODO: Phase 1 — create Picture widget with the gtk4paintablesink paintable
        // and swap it in, then go fullscreen
        info!("Switching to video display mode");
    }

    pub fn present(&self) {
        self.window.present();
    }
}
