use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use openplay_common::AppConfig;
use tracing::info;

use crate::window::ReceiverWindow;

const APP_ID: &str = "org.openplay.Receiver";

/// Runs the GTK4 receiver application and returns the exit code.
pub fn run(config: AppConfig) -> i32 {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        let window = ReceiverWindow::new(app, &config);
        window.present();
    });

    app.run().into()
}
