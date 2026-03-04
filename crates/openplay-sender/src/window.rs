use std::cell::RefCell;
use std::collections::HashMap;

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use openplay_common::AppConfig;
use tracing::info;

use crate::casting::CastStopHandle;
use crate::receiver_list::{DiscoveredReceiver, MiracastMode, Protocol};

/// Main window for the OpenPlay sender application.
pub struct SenderWindow {
    window: adw::ApplicationWindow,
    receiver_list: gtk::ListBox,
    cast_button: gtk::Button,
    stop_button: gtk::Button,
    add_miracast_button: gtk::Button,
    status_label: gtk::Label,
    receivers: RefCell<HashMap<String, DiscoveredReceiver>>,
    /// Active cast stop handle — set when casting starts, cleared when idle.
    cast_stop_handle: RefCell<Option<CastStopHandle>>,
}

impl SenderWindow {
    pub fn new(app: &adw::Application, _config: &AppConfig) -> Self {
        let header = adw::HeaderBar::new();

        // Receiver list (will be populated by discovery)
        let receiver_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(vec!["boxed-list"])
            .build();

        // Placeholder when no receivers found
        receiver_list.set_placeholder(Some(
            &gtk::Label::builder()
                .label("Searching for receivers...")
                .margin_top(20)
                .margin_bottom(20)
                .css_classes(vec!["dim-label"])
                .build(),
        ));

        // Cast button
        let cast_button = gtk::Button::builder()
            .label("Start Casting")
            .css_classes(vec!["suggested-action", "pill"])
            .halign(gtk::Align::Center)
            .sensitive(false)
            .build();

        // Stop button (hidden until casting)
        let stop_button = gtk::Button::builder()
            .label("Stop Casting")
            .css_classes(vec!["destructive-action", "pill"])
            .halign(gtk::Align::Center)
            .visible(false)
            .build();

        // Add Miracast receiver button (manual IP entry)
        let add_miracast_button = gtk::Button::builder()
            .label("Add Miracast Receiver")
            .css_classes(vec!["flat"])
            .halign(gtk::Align::Center)
            .build();

        // Status label for casting progress
        let status_label = gtk::Label::builder()
            .css_classes(vec!["dim-label"])
            .halign(gtk::Align::Center)
            .visible(false)
            .build();

        // Enable cast button when a receiver is selected
        let cast_btn_clone = cast_button.clone();
        receiver_list.connect_row_selected(move |_, row| {
            cast_btn_clone.set_sensitive(row.is_some());
        });

        // Layout
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(20)
            .margin_start(20)
            .margin_end(20)
            .margin_top(10)
            .margin_bottom(20)
            .build();

        let receivers_label = gtk::Label::builder()
            .label("Available Receivers")
            .css_classes(vec!["title-4"])
            .halign(gtk::Align::Start)
            .build();

        content.append(&receivers_label);
        content.append(&receiver_list);
        content.append(&add_miracast_button);
        content.append(&cast_button);
        content.append(&stop_button);
        content.append(&status_label);

        let scrolled = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&content)
            .build();

        let toolbar_view = adw::ToolbarView::builder().build();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&scrolled));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("OpenPlay")
            .default_width(400)
            .default_height(500)
            .content(&toolbar_view)
            .build();

        info!("Sender window created");

        Self {
            window,
            receiver_list,
            cast_button,
            stop_button,
            add_miracast_button,
            status_label,
            receivers: RefCell::new(HashMap::new()),
            cast_stop_handle: RefCell::new(None),
        }
    }

    /// Adds a discovered receiver to the list.
    pub fn add_receiver(&self, receiver: &DiscoveredReceiver) {
        let key = receiver.key();

        // Avoid duplicate entries
        if self.receivers.borrow().contains_key(&key) {
            return;
        }

        self.receivers
            .borrow_mut()
            .insert(key.clone(), receiver.clone());
        let row = build_receiver_row(receiver);
        self.receiver_list.append(&row);
    }

    /// Removes a receiver from the list by key.
    pub fn remove_receiver(&self, key: &str) {
        self.receivers.borrow_mut().remove(key);
        let mut idx = 0;
        while let Some(row) = self.receiver_list.row_at_index(idx) {
            if let Some(name) = row.widget_name().strip_prefix("receiver-") {
                if name == key {
                    self.receiver_list.remove(&row);
                    return;
                }
            }
            idx += 1;
        }
    }

    /// Returns the currently selected receiver, if any.
    pub fn selected_receiver(&self) -> Option<DiscoveredReceiver> {
        let row = self.receiver_list.selected_row()?;
        let widget_name = row.widget_name();
        let key = widget_name.strip_prefix("receiver-")?;
        self.receivers.borrow().get(key).cloned()
    }

    /// Returns the cast button widget.
    pub fn cast_button(&self) -> &gtk::Button {
        &self.cast_button
    }

    /// Returns the stop button widget.
    pub fn stop_button(&self) -> &gtk::Button {
        &self.stop_button
    }

    /// Returns the "Add Miracast" button widget.
    pub fn add_miracast_button(&self) -> &gtk::Button {
        &self.add_miracast_button
    }

    /// Returns the parent window (for dialog transient parent).
    pub fn gtk_window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    /// Creates a new stop handle and stores it. Returns the handle for passing to cast functions.
    pub fn create_stop_handle(&self) -> CastStopHandle {
        let handle = CastStopHandle::new();
        *self.cast_stop_handle.borrow_mut() = Some(handle.clone());
        handle
    }

    /// Signals the active casting session to stop.
    pub fn stop_casting(&self) {
        if let Some(handle) = self.cast_stop_handle.borrow().as_ref() {
            handle.stop();
            info!("Stop signal sent to casting session");
        }
    }

    /// Switch UI to "casting" state.
    pub fn set_casting_state(&self) {
        self.cast_button.set_sensitive(false);
        self.cast_button.set_label("Casting...");
        self.cast_button.set_visible(false);
        self.stop_button.set_visible(true);
        self.add_miracast_button.set_sensitive(false);
    }

    /// Switch UI back to "idle" state.
    pub fn set_idle_state(&self) {
        self.cast_button.set_sensitive(true);
        self.cast_button.set_label("Start Casting");
        self.cast_button.set_visible(true);
        self.stop_button.set_visible(false);
        self.add_miracast_button.set_sensitive(true);
        // Clear the stop handle
        *self.cast_stop_handle.borrow_mut() = None;
    }

    /// Sets the status text (shown below cast button).
    pub fn set_status(&self, text: &str) {
        self.status_label.set_label(text);
        self.status_label.set_visible(!text.is_empty());
    }

    pub fn present(&self) {
        self.window.present();
    }
}

/// Builds a ListBox row for a discovered receiver with protocol badge.
fn build_receiver_row(receiver: &DiscoveredReceiver) -> adw::ActionRow {
    let protocol_badge = gtk::Label::builder()
        .label(receiver.protocol_label())
        .css_classes(vec![
            "caption",
            protocol_css_class(receiver.protocol()),
        ])
        .valign(gtk::Align::Center)
        .build();

    let row = adw::ActionRow::builder()
        .title(receiver.display_name())
        .subtitle(&format_subtitle(receiver))
        .activatable(true)
        .build();

    row.add_suffix(&protocol_badge);
    row.set_widget_name(&format!("receiver-{}", receiver.key()));

    row
}

fn format_subtitle(receiver: &DiscoveredReceiver) -> String {
    match receiver {
        DiscoveredReceiver::OpenPlay(r) => {
            let addr = r
                .addresses
                .first()
                .map(|a| a.to_string())
                .unwrap_or_default();
            format!("{addr}:{}", r.port)
        }
        DiscoveredReceiver::AirPlay(r) => {
            let addr = r
                .addresses
                .first()
                .map(|a| a.to_string())
                .unwrap_or_default();
            if r.model.is_empty() {
                addr
            } else {
                format!("{} \u{2022} {addr}", r.model)
            }
        }
        DiscoveredReceiver::Miracast(r) => match &r.mode {
            MiracastMode::Infrastructure { addr, port } => format!("{addr}:{port}"),
            MiracastMode::WifiDirect { device_address } => format!("P2P {device_address}"),
        }
    }
}

fn protocol_css_class(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::OpenPlay => "accent",
        Protocol::AirPlay => "success",
        Protocol::Miracast => "warning",
    }
}
