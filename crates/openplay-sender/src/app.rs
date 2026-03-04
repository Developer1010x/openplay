use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;
use libadwaita as adw;
use openplay_common::AppConfig;
use openplay_discovery::{AirPlayBrowser, DiscoveryEvent, MiracastBrowser, ReceiverBrowser};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::casting;
use crate::receiver_list::{DiscoveredReceiver, MiracastMode, MiracastReceiver, Protocol};
use crate::window::SenderWindow;

const APP_ID: &str = "org.openplay.Sender";

/// Runs the GTK4 application and returns the exit code.
pub fn run(config: AppConfig) -> i32 {
    // Create tokio runtime for async networking (AirPlay sessions, etc.)
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");
    let tokio_handle = tokio_rt.handle().clone();

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_activate(move |app| {
        let window = Rc::new(SenderWindow::new(app, &config));

        // Start discovery browsers
        let (merged_tx, merged_rx) = mpsc::channel::<DiscoveryEvent>(64);

        // OpenPlay browser
        match ReceiverBrowser::start() {
            Ok((_browser, mut openplay_rx)) => {
                let tx = merged_tx.clone();
                glib::spawn_future_local(async move {
                    while let Some(event) = openplay_rx.recv().await {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                });
                info!("OpenPlay browser started");
            }
            Err(e) => {
                warn!("Failed to start OpenPlay browser: {e}");
            }
        }

        // AirPlay browser (if enabled)
        if config.airplay_enabled {
            match AirPlayBrowser::start() {
                Ok((_browser, mut airplay_rx)) => {
                    let tx = merged_tx.clone();
                    glib::spawn_future_local(async move {
                        while let Some(event) = airplay_rx.recv().await {
                            if tx.send(event).await.is_err() {
                                break;
                            }
                        }
                    });
                    info!("AirPlay browser started");
                }
                Err(e) => {
                    warn!("Failed to start AirPlay browser: {e}");
                }
            }
        }

        // Miracast MICE browser (if enabled) — auto-detects Miracast displays on the network
        if config.miracast_enabled {
            match MiracastBrowser::start() {
                Ok((_browser, mut miracast_rx)) => {
                    let tx = merged_tx.clone();
                    glib::spawn_future_local(async move {
                        while let Some(event) = miracast_rx.recv().await {
                            if tx.send(event).await.is_err() {
                                break;
                            }
                        }
                    });
                    info!("Miracast MICE browser started — auto-detecting displays");
                }
                Err(e) => {
                    warn!("Failed to start Miracast MICE browser: {e}");
                }
            }
        }

        // Miracast Wi-Fi Direct discovery (if enabled)
        if config.miracast_enabled {
            let window_for_wfd = window.clone();
            let handle_for_wfd = tokio_handle.clone();
            glib::spawn_future_local(async move {
                match handle_for_wfd.spawn(async {
                    openplay_miracast::wifi_direct::WifiDirectManager::start().await
                }).await {
                    Ok(Ok((_manager, mut events))) => {
                        info!("Wi-Fi Direct discovery started for Miracast");
                        while let Some(event) = events.recv().await {
                            match event {
                                openplay_miracast::wifi_direct::WifiDirectEvent::PeerFound(peer) => {
                                    let receiver = DiscoveredReceiver::Miracast(MiracastReceiver {
                                        display_name: peer.name.clone(),
                                        mode: MiracastMode::WifiDirect {
                                            device_address: peer.device_address.clone(),
                                        },
                                    });
                                    window_for_wfd.add_receiver(&receiver);
                                }
                                openplay_miracast::wifi_direct::WifiDirectEvent::PeerLost { device_address } => {
                                    window_for_wfd.remove_receiver(&format!("miracast-p2p-{device_address}"));
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        warn!("Wi-Fi Direct discovery not available: {e}");
                    }
                    Err(e) => {
                        warn!("Wi-Fi Direct discovery task failed: {e}");
                    }
                }
            });
        }

        // Process merged discovery events
        let window_for_discovery = window.clone();
        glib::spawn_future_local(async move {
            let mut rx = merged_rx;
            while let Some(event) = rx.recv().await {
                match event {
                    DiscoveryEvent::ReceiverFound(info) => {
                        let receiver = DiscoveredReceiver::OpenPlay(info);
                        window_for_discovery.add_receiver(&receiver);
                    }
                    DiscoveryEvent::ReceiverLost { name } => {
                        window_for_discovery.remove_receiver(&name);
                    }
                    DiscoveryEvent::AirPlayReceiverFound(info) => {
                        let receiver = DiscoveredReceiver::AirPlay(info);
                        window_for_discovery.add_receiver(&receiver);
                    }
                    DiscoveryEvent::AirPlayReceiverLost { name } => {
                        window_for_discovery.remove_receiver(&name);
                    }
                    DiscoveryEvent::MiracastReceiverFound(info) => {
                        // Convert MICE discovery info to our receiver model
                        let addr = info
                            .addresses
                            .first()
                            .copied()
                            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
                        let receiver = DiscoveredReceiver::Miracast(MiracastReceiver {
                            display_name: info.display_name,
                            mode: MiracastMode::Infrastructure {
                                addr,
                                port: info.port,
                            },
                        });
                        window_for_discovery.add_receiver(&receiver);
                    }
                    DiscoveryEvent::MiracastReceiverLost { name } => {
                        window_for_discovery.remove_receiver(&name);
                    }
                }
            }
        });

        // Wire up cast button
        let window_for_cast = window.clone();
        let handle = tokio_handle.clone();
        let cast_config = config.clone();
        window.cast_button().connect_clicked(move |_| {
            let Some(receiver) = window_for_cast.selected_receiver() else {
                window_for_cast.set_status("No receiver selected");
                return;
            };

            info!(
                name = receiver.display_name(),
                protocol = receiver.protocol_label(),
                "Cast button clicked"
            );

            // Common setup: switch to casting state and create stop handle
            window_for_cast.set_casting_state();
            let stop_handle = window_for_cast.create_stop_handle();

            let handle = handle.clone();
            let bitrate = cast_config.max_bitrate_kbps;
            let fps = cast_config.framerate;

            // Status callback — updates UI and resets on completion/error
            let window_for_status = window_for_cast.clone();
            let make_status_cb = move || {
                let w = window_for_status.clone();
                move |status: &str| {
                    let status = status.to_string();
                    let w = w.clone();
                    glib::idle_add_local_once(move || {
                        w.set_status(&status);
                        let s = status.to_lowercase();
                        if s.contains("failed") || s.contains("error")
                            || s.contains("ended") || s.contains("stopped")
                        {
                            w.set_idle_state();
                        }
                    });
                }
            };

            match receiver.protocol() {
                Protocol::AirPlay => {
                    let Some(addr) = receiver.addr() else {
                        window_for_cast.set_status("No address for receiver");
                        window_for_cast.set_idle_state();
                        return;
                    };

                    window_for_cast.set_status("Starting screen capture...");
                    let status_cb = make_status_cb();

                    glib::spawn_future_local(async move {
                        casting::start_airplay_cast(
                            addr, bitrate, fps, handle, stop_handle, status_cb,
                        ).await;
                    });
                }
                Protocol::OpenPlay => {
                    window_for_cast.set_status("OpenPlay casting not yet wired up");
                    window_for_cast.set_idle_state();
                }
                Protocol::Miracast => {
                    if receiver.is_wifi_direct() {
                        let Some(mac) = receiver.wifi_direct_address() else {
                            window_for_cast.set_status("No P2P address for receiver");
                            window_for_cast.set_idle_state();
                            return;
                        };
                        window_for_cast.set_status("Connecting via Wi-Fi Direct...");
                        let mac = mac.to_string();
                        let status_cb = make_status_cb();
                        glib::spawn_future_local(async move {
                            casting::start_miracast_p2p_cast(
                                &mac, bitrate, fps, handle, stop_handle, status_cb,
                            ).await;
                        });
                    } else {
                        let Some(addr) = receiver.addr() else {
                            window_for_cast.set_status("No address for Miracast receiver");
                            window_for_cast.set_idle_state();
                            return;
                        };
                        window_for_cast.set_status("Starting Miracast...");
                        let status_cb = make_status_cb();
                        glib::spawn_future_local(async move {
                            casting::start_miracast_cast(
                                addr.ip(), bitrate, fps, handle, stop_handle, status_cb,
                            ).await;
                        });
                    }
                }
            }
        });

        // Wire up "Add Miracast Receiver" button — supports MICE (IP) and P2P (MAC)
        let window_for_miracast = window.clone();
        window.add_miracast_button().connect_clicked(move |_| {
            let dialog = gtk::Dialog::builder()
                .title("Add Miracast Receiver")
                .transient_for(window_for_miracast.gtk_window())
                .modal(true)
                .build();

            dialog.add_button("Cancel", gtk::ResponseType::Cancel);
            dialog.add_button("Add", gtk::ResponseType::Accept);

            let content_area = dialog.content_area();
            content_area.set_spacing(12);
            content_area.set_margin_start(20);
            content_area.set_margin_end(20);
            content_area.set_margin_top(12);
            content_area.set_margin_bottom(12);

            let name_entry = gtk::Entry::builder()
                .placeholder_text("Display Name (e.g. Living Room TV)")
                .text("Miracast Receiver")
                .build();

            // Mode selector: Infrastructure (IP) or Wi-Fi Direct (MAC)
            let mode_label = gtk::Label::builder()
                .label("Connection Mode:")
                .halign(gtk::Align::Start)
                .build();

            let mode_combo = gtk::ComboBoxText::new();
            mode_combo.append_text("Wi-Fi (IP Address)");
            mode_combo.append_text("Wi-Fi Direct (MAC Address)");
            mode_combo.set_active(Some(0));

            let addr_entry = gtk::Entry::builder()
                .placeholder_text("IP Address (e.g. 192.168.0.100)")
                .build();

            // Update placeholder based on mode
            let addr_entry_for_mode = addr_entry.clone();
            mode_combo.connect_changed(move |combo| {
                if combo.active() == Some(1) {
                    addr_entry_for_mode.set_placeholder_text(Some("MAC Address (e.g. AA:BB:CC:DD:EE:FF)"));
                } else {
                    addr_entry_for_mode.set_placeholder_text(Some("IP Address (e.g. 192.168.0.100)"));
                }
            });

            content_area.append(&gtk::Label::builder().label("Name:").halign(gtk::Align::Start).build());
            content_area.append(&name_entry);
            content_area.append(&mode_label);
            content_area.append(&mode_combo);
            content_area.append(&gtk::Label::builder().label("Address:").halign(gtk::Align::Start).build());
            content_area.append(&addr_entry);

            let w = window_for_miracast.clone();
            dialog.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept {
                    let addr_text = addr_entry.text().to_string().trim().to_string();
                    let name_text = name_entry.text().to_string();
                    let display_name = if name_text.is_empty() { addr_text.clone() } else { name_text };
                    let is_p2p = mode_combo.active() == Some(1);

                    if is_p2p {
                        // Wi-Fi Direct mode — MAC address
                        if addr_text.len() >= 11 && addr_text.contains(':') {
                            let receiver = DiscoveredReceiver::Miracast(MiracastReceiver {
                                display_name,
                                mode: MiracastMode::WifiDirect { device_address: addr_text.clone() },
                            });
                            w.add_receiver(&receiver);
                            info!(mac = %addr_text, "Added P2P Miracast receiver");
                        } else {
                            w.set_status("Invalid MAC address (e.g. AA:BB:CC:DD:EE:FF)");
                        }
                    } else {
                        // Infrastructure mode — IP address
                        if let Ok(ip) = addr_text.parse::<std::net::IpAddr>() {
                            let receiver = DiscoveredReceiver::Miracast(MiracastReceiver {
                                display_name,
                                mode: MiracastMode::Infrastructure { addr: ip, port: 7236 },
                            });
                            w.add_receiver(&receiver);
                            info!(ip = %addr_text, "Added MICE Miracast receiver");
                        } else {
                            w.set_status("Invalid IP address");
                        }
                    }
                }
                dialog.close();
            });

            dialog.present();
        });

        // Wire up stop button — signals the active cast to stop
        let window_for_stop = window.clone();
        window.stop_button().connect_clicked(move |_| {
            window_for_stop.set_status("Stopping...");
            window_for_stop.stop_casting();
            info!("Stop button clicked — signaling cast to stop");
        });

        window.present();
    });

    // Keep tokio runtime alive for the lifetime of the app
    let _rt_guard = tokio_rt;
    app.run().into()
}
