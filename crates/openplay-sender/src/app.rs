use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::mpsc as std_mpsc;

use egui::{Color32, RichText, ScrollArea, Vec2};
use openplay_common::AppConfig;
use openplay_discovery::{AirPlayBrowser, DiscoveryEvent, MiracastBrowser, ReceiverBrowser};
use tracing::{info, warn};

use crate::casting::{start_airplay_cast, start_miracast_cast, CastStopHandle};
use crate::receiver_list::{DiscoveredReceiver, MiracastMode, MiracastReceiver, Protocol};

// ─── App state ────────────────────────────────────────────────────────────────

pub struct SenderApp {
    // Config
    config: AppConfig,
    // Tokio runtime for async networking
    tokio_rt: tokio::runtime::Runtime,

    // Discovery
    event_rx: std_mpsc::Receiver<DiscoveryEvent>,
    receivers: HashMap<String, DiscoveredReceiver>,
    receiver_keys: Vec<String>, // ordered

    // Selection
    selected_key: Option<String>,

    // Casting state
    status: String,
    is_casting: bool,
    stop_handle: Option<CastStopHandle>,
    /// Whether we stopped the Wi-Fi Direct Find scan for the current cast, so
    /// that we only resume a scan we ourselves paused. Linux-only, matching
    /// `wifi_direct`, which drives wpa_supplicant over D-Bus.
    #[cfg(target_os = "linux")]
    p2p_discovery_paused: bool,
    status_rx: std_mpsc::Receiver<String>,
    status_tx: std_mpsc::SyncSender<String>,

    // Dialogs
    show_add_airplay: bool,
    airplay_name: String,
    airplay_ip: String,
    airplay_port: String,

    show_add_miracast: bool,
    miracast_name: String,
    miracast_addr: String,
    miracast_is_p2p: bool,
}

impl SenderApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, config: AppConfig) -> Self {
        let tokio_rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        // Merge all discovery events into one sync channel
        let (event_tx, event_rx) = std_mpsc::sync_channel::<DiscoveryEvent>(128);
        let (status_tx, status_rx) = std_mpsc::sync_channel::<String>(32);

        // Start OpenPlay mDNS browser
        match ReceiverBrowser::start() {
            Ok((_browser, mut rx)) => {
                let tx = event_tx.clone();
                tokio_rt.spawn(async move {
                    while let Some(e) = rx.recv().await {
                        let _ = tx.send(e);
                    }
                });
                info!("OpenPlay browser started");
            }
            Err(e) => warn!("OpenPlay browser failed: {e}"),
        }

        // Start AirPlay mDNS browser
        if config.airplay_enabled {
            match AirPlayBrowser::start() {
                Ok((_browser, mut rx)) => {
                    let tx = event_tx.clone();
                    tokio_rt.spawn(async move {
                        while let Some(e) = rx.recv().await {
                            let _ = tx.send(e);
                        }
                    });
                    info!("AirPlay browser started");
                }
                Err(e) => warn!("AirPlay browser failed: {e}"),
            }
        }

        // Start Miracast mDNS browser
        if config.miracast_enabled {
            match MiracastBrowser::start() {
                Ok((_browser, mut rx)) => {
                    let tx = event_tx.clone();
                    tokio_rt.spawn(async move {
                        while let Some(e) = rx.recv().await {
                            let _ = tx.send(e);
                        }
                    });
                    info!("Miracast browser started (all service types)");
                }
                Err(e) => warn!("Miracast browser failed: {e}"),
            }
        }

        // Start Wi-Fi Direct discovery (Linux only)
        #[cfg(target_os = "linux")]
        if config.miracast_enabled {
            let tx = event_tx.clone();
            tokio_rt.spawn(async move {
                match openplay_miracast::wifi_direct::WifiDirectManager::start().await {
                    Ok((_mgr, mut events)) => {
                        info!("Wi-Fi Direct discovery started");
                        while let Some(ev) = events.recv().await {
                            use openplay_miracast::wifi_direct::WifiDirectEvent;
                            match ev {
                                WifiDirectEvent::PeerFound(peer) => {
                                    let _ = tx.send(DiscoveryEvent::MiracastReceiverFound(
                                        openplay_discovery::MiracastReceiverInfo {
                                            name: format!("miracast-p2p-{}", peer.device_address),
                                            display_name: peer.name.clone(),
                                            addresses: vec![],
                                            port: 7236,
                                            device_info: peer.device_address.clone(),
                                        },
                                    ));
                                }
                                WifiDirectEvent::PeerLost { device_address } => {
                                    let _ = tx.send(DiscoveryEvent::MiracastReceiverLost {
                                        name: format!("miracast-p2p-{device_address}"),
                                    });
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => warn!("Wi-Fi Direct not available: {e}"),
                }
            });
        }

        Self {
            config,
            tokio_rt,
            event_rx,
            receivers: HashMap::new(),
            receiver_keys: Vec::new(),
            selected_key: None,
            status: "Searching for receivers...".to_string(),
            is_casting: false,
            stop_handle: None,
            #[cfg(target_os = "linux")]
            p2p_discovery_paused: false,
            status_rx,
            status_tx,
            show_add_airplay: false,
            airplay_name: String::new(),
            airplay_ip: String::new(),
            airplay_port: "7000".to_string(),
            show_add_miracast: false,
            miracast_name: String::new(),
            miracast_addr: String::new(),
            miracast_is_p2p: false,
        }
    }

    fn poll_discovery(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                DiscoveryEvent::ReceiverFound(info) => {
                    let r = DiscoveredReceiver::OpenPlay(info);
                    self.add_receiver(r);
                }
                DiscoveryEvent::ReceiverLost { name } => self.remove_receiver(&name),
                DiscoveryEvent::AirPlayReceiverFound(info) => {
                    let r = DiscoveredReceiver::AirPlay(info);
                    self.add_receiver(r);
                }
                DiscoveryEvent::AirPlayReceiverLost { name } => self.remove_receiver(&name),
                DiscoveryEvent::MiracastReceiverFound(info) => {
                    let addr = info
                        .addresses
                        .first()
                        .copied()
                        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
                    // Check if device_info looks like a MAC (Wi-Fi Direct) or IP
                    let mode = if info.device_info.contains(':') && info.device_info.len() == 17 {
                        MiracastMode::WifiDirect {
                            device_address: info.device_info.clone(),
                        }
                    } else {
                        MiracastMode::Infrastructure {
                            addr,
                            port: info.port,
                        }
                    };
                    let r = DiscoveredReceiver::Miracast(MiracastReceiver {
                        display_name: info.display_name,
                        mode,
                    });
                    self.add_receiver(r);
                }
                DiscoveryEvent::MiracastReceiverLost { name } => self.remove_receiver(&name),
            }
        }
    }

    fn poll_status(&mut self) {
        while let Ok(msg) = self.status_rx.try_recv() {
            let done = msg.to_lowercase().contains("ended")
                || msg.to_lowercase().contains("failed")
                || msg.to_lowercase().contains("stopped")
                || msg.to_lowercase().contains("error");
            self.status = msg;
            if done {
                self.is_casting = false;
                self.stop_handle = None;
                #[cfg(target_os = "linux")]
                self.resume_p2p_discovery();
            }
        }
    }

    fn add_receiver(&mut self, r: DiscoveredReceiver) {
        let key = r.key();
        if !self.receivers.contains_key(&key) {
            self.receiver_keys.push(key.clone());
            self.receivers.insert(key, r);
        }
    }

    fn remove_receiver(&mut self, key: &str) {
        if self.receivers.remove(key).is_some() {
            self.receiver_keys.retain(|k| k != key);
            if self.selected_key.as_deref() == Some(key) {
                self.selected_key = None;
            }
        }
    }

    /// Stop the Wi-Fi Direct Find scan for the duration of a cast that does not
    /// need it.
    ///
    /// Find sweeps the radio off its associated channel, which shows up on the
    /// LAN as latency spikes and, on connect, as `No route to host`. Every
    /// protocol except Wi-Fi Direct P2P runs over that same interface, so every
    /// one of them wants the scan quiet.
    #[cfg(target_os = "linux")]
    fn pause_p2p_discovery(&mut self) {
        if self.p2p_discovery_paused {
            return;
        }
        self.p2p_discovery_paused = true;
        self.tokio_rt.spawn(async {
            if let Err(e) = openplay_miracast::wifi_direct::pause_discovery().await {
                warn!("Could not pause Wi-Fi Direct discovery: {e}");
            }
        });
    }

    /// Restart the Find scan paused by [`Self::pause_p2p_discovery`], so peers
    /// reappear once the cast is over.
    #[cfg(target_os = "linux")]
    fn resume_p2p_discovery(&mut self) {
        if !self.p2p_discovery_paused {
            return;
        }
        self.p2p_discovery_paused = false;
        self.tokio_rt.spawn(async {
            if let Err(e) = openplay_miracast::wifi_direct::resume_discovery().await {
                warn!("Could not resume Wi-Fi Direct discovery: {e}");
            }
        });
    }

    fn start_cast(&mut self, receiver: DiscoveredReceiver) {
        // A P2P cast needs Find active for GO negotiation; everything else is
        // damaged by it. Decide before any pipeline is built.
        #[cfg(target_os = "linux")]
        if !receiver.is_wifi_direct() {
            self.pause_p2p_discovery();
        }

        let bitrate = self.config.max_bitrate_kbps;
        let fps = self.config.framerate;
        let force_sw = self.config.force_sw_encode;
        let handle = self.tokio_rt.handle().clone();
        let stop = CastStopHandle::new();
        self.stop_handle = Some(stop.clone());
        self.is_casting = true;
        self.status = "Starting capture...".to_string();

        let tx = self.status_tx.clone();
        let status_cb = move |msg: &str| {
            let _ = tx.send(msg.to_string());
        };

        match receiver.protocol() {
            Protocol::AirPlay => {
                if let Some(addr) = receiver.addr() {
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(start_airplay_cast(
                            addr, bitrate, fps, force_sw, handle, stop, status_cb,
                        ));
                    });
                }
            }
            Protocol::Miracast => {
                if receiver.is_wifi_direct() {
                    #[cfg(target_os = "linux")]
                    if let Some(mac) = receiver.wifi_direct_address() {
                        let mac = mac.to_string();
                        let tx2 = self.status_tx.clone();
                        let stop2 = self.stop_handle.clone().unwrap();
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .unwrap();
                            rt.block_on(crate::casting::start_miracast_p2p_cast(
                                &mac,
                                bitrate,
                                fps,
                                force_sw,
                                handle,
                                stop2,
                                move |m| {
                                    let _ = tx2.send(m.to_string());
                                },
                            ));
                        });
                    }
                    #[cfg(not(target_os = "linux"))]
                    {
                        self.status = "Wi-Fi Direct P2P is only available on Linux".to_string();
                        self.is_casting = false;
                    }
                } else if let Some(addr) = receiver.addr() {
                    std::thread::spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(start_miracast_cast(
                            addr.ip(),
                            bitrate,
                            fps,
                            force_sw,
                            handle,
                            stop,
                            status_cb,
                        ));
                    });
                }
            }
            Protocol::OpenPlay => {
                self.status = "OpenPlay WebRTC casting: connecting...".to_string();
                // TODO: implement OpenPlay WebRTC casting path
                self.is_casting = false;
            }
        }
    }

    fn stop_cast(&mut self) {
        if let Some(h) = &self.stop_handle {
            h.stop();
        }
        self.status = "Stopping...".to_string();
    }
}

// ─── egui App impl ─────────────────────────────────────────────────────────────

impl eframe::App for SenderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_discovery();
        self.poll_status();

        // Always repaint while casting (to pick up status updates)
        if self.is_casting {
            ctx.request_repaint();
        }

        // ── Top panel (title bar) ──────────────────────────────────────────────
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("OpenPlay").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("● {}", self.config.display_name))
                            .small()
                            .color(Color32::GRAY),
                    );
                });
            });
            ui.add_space(4.0);
        });

        // ── Status bar (bottom) ───────────────────────────────────────────────
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if self.is_casting {
                    ui.spinner();
                    ui.label(RichText::new(&self.status).color(Color32::from_rgb(100, 200, 100)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(
                                RichText::new("⏹ Stop Casting")
                                    .color(Color32::from_rgb(255, 80, 80)),
                            ))
                            .clicked()
                        {
                            self.stop_cast();
                        }
                    });
                } else {
                    ui.label(RichText::new(&self.status).color(Color32::GRAY).small());
                }
            });
            ui.add_space(4.0);
        });

        // ── Main panel ────────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            // Receiver list header + add buttons
            ui.horizontal(|ui| {
                ui.label(RichText::new("Available Receivers").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("+ Miracast IP").clicked() {
                        self.show_add_miracast = true;
                    }
                    if ui.small_button("+ AirPlay IP").clicked() {
                        self.show_add_airplay = true;
                    }
                });
            });
            ui.separator();

            // Receiver list
            if self.receiver_keys.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Scanning for receivers...").color(Color32::GRAY));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Supports: AirPlay (Apple TV, LG, Samsung, NOVO)\n\
                             Miracast (NOVO Pro, LG CreateBoard, ViewSonic)\n\
                             OpenPlay (native, free)",
                        )
                        .small()
                        .color(Color32::DARK_GRAY),
                    );
                });
            } else {
                ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    let keys: Vec<_> = self.receiver_keys.clone();
                    for key in &keys {
                        let Some(receiver) = self.receivers.get(key) else {
                            continue;
                        };
                        let selected = self.selected_key.as_deref() == Some(key);
                        let (proto_label, proto_color) = protocol_badge(receiver.protocol());

                        // The row cards are painted a fixed dark colour whatever the
                        // app theme is, so their text colour has to be fixed too.
                        // Inheriting the theme's default text colour put near-black
                        // text on a near-black card in light mode.
                        let (row_fill, name_colour) = if selected {
                            (Color32::from_rgb(40, 80, 140), Color32::WHITE)
                        } else {
                            (Color32::from_gray(30), Color32::from_gray(235))
                        };

                        let response = egui::Frame::none()
                            .fill(row_fill)
                            .rounding(6.0)
                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(
                                            RichText::new(receiver.display_name())
                                                .strong()
                                                .color(name_colour),
                                        );
                                        ui.label(
                                            RichText::new(receiver_subtitle(receiver))
                                                .small()
                                                .color(Color32::GRAY),
                                        );
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new(proto_label)
                                                    .small()
                                                    .color(proto_color),
                                            );
                                        },
                                    );
                                });
                            });

                        if response.response.interact(egui::Sense::click()).clicked() {
                            self.selected_key = Some(key.clone());
                        }
                        ui.add_space(3.0);
                    }
                });
            }

            ui.add_space(16.0);

            // Cast button
            if !self.is_casting {
                let can_cast = self.selected_key.is_some();
                ui.vertical_centered(|ui| {
                    let btn = egui::Button::new(
                        RichText::new("▶  Start Casting")
                            .size(16.0)
                            .color(Color32::WHITE),
                    )
                    .fill(if can_cast {
                        Color32::from_rgb(40, 140, 60)
                    } else {
                        Color32::DARK_GRAY
                    })
                    .min_size(Vec2::new(200.0, 42.0));

                    if ui.add_enabled(can_cast, btn).clicked() {
                        if let Some(key) = &self.selected_key.clone() {
                            if let Some(receiver) = self.receivers.get(key).cloned() {
                                self.start_cast(receiver);
                            }
                        }
                    }
                });
            }

            ui.add_space(8.0);
            ui.separator();

            // Settings summary
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Bitrate: {} kbps   FPS: {}",
                        self.config.max_bitrate_kbps, self.config.framerate
                    ))
                    .small()
                    .color(Color32::DARK_GRAY),
                );
            });
        });

        // ── Add AirPlay IP dialog ─────────────────────────────────────────────
        if self.show_add_airplay {
            egui::Window::new("Add AirPlay Receiver")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.airplay_name);
                    ui.label("IP Address:");
                    ui.text_edit_singleline(&mut self.airplay_ip);
                    ui.label("Port:");
                    ui.text_edit_singleline(&mut self.airplay_port);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            if let Ok(ip) = IpAddr::from_str(&self.airplay_ip) {
                                let port: u16 = self.airplay_port.parse().unwrap_or(7000);
                                let name = if self.airplay_name.is_empty() {
                                    self.airplay_ip.clone()
                                } else {
                                    self.airplay_name.clone()
                                };
                                let receiver = DiscoveredReceiver::AirPlay(
                                    openplay_discovery::AirPlayReceiverInfo {
                                        name: format!("manual-airplay-{ip}"),
                                        display_name: name,
                                        addresses: vec![ip],
                                        port,
                                        device_id: String::new(),
                                        features: String::new(),
                                        model: "Manual".to_string(),
                                    },
                                );
                                self.add_receiver(receiver);
                                info!(ip = %ip, port, "Added manual AirPlay receiver");
                            } else {
                                self.status = "Invalid IP address".to_string();
                            }
                            self.show_add_airplay = false;
                            self.airplay_name.clear();
                            self.airplay_ip.clear();
                            self.airplay_port = "7000".to_string();
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_add_airplay = false;
                        }
                    });
                });
        }

        // ── Add Miracast IP dialog ────────────────────────────────────────────
        if self.show_add_miracast {
            egui::Window::new("Add Miracast Receiver")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.miracast_name);
                    ui.checkbox(&mut self.miracast_is_p2p, "Wi-Fi Direct (MAC address)");
                    ui.label(if self.miracast_is_p2p {
                        "MAC Address (AA:BB:CC:DD:EE:FF):"
                    } else {
                        "IP Address:"
                    });
                    ui.text_edit_singleline(&mut self.miracast_addr);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            let name = if self.miracast_name.is_empty() {
                                self.miracast_addr.clone()
                            } else {
                                self.miracast_name.clone()
                            };
                            if self.miracast_is_p2p {
                                if self.miracast_addr.len() >= 11
                                    && self.miracast_addr.contains(':')
                                {
                                    let r = DiscoveredReceiver::Miracast(MiracastReceiver {
                                        display_name: name,
                                        mode: MiracastMode::WifiDirect {
                                            device_address: self.miracast_addr.clone(),
                                        },
                                    });
                                    self.add_receiver(r);
                                } else {
                                    self.status = "Invalid MAC address".to_string();
                                }
                            } else if let Ok(ip) = IpAddr::from_str(&self.miracast_addr) {
                                let r = DiscoveredReceiver::Miracast(MiracastReceiver {
                                    display_name: name,
                                    mode: MiracastMode::Infrastructure {
                                        addr: ip,
                                        port: 7236,
                                    },
                                });
                                self.add_receiver(r);
                                info!(ip = %ip, "Added manual Miracast receiver");
                            } else {
                                self.status = "Invalid IP address".to_string();
                            }
                            self.show_add_miracast = false;
                            self.miracast_name.clear();
                            self.miracast_addr.clear();
                            self.miracast_is_p2p = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_add_miracast = false;
                        }
                    });
                });
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn protocol_badge(protocol: Protocol) -> (&'static str, Color32) {
    let color = match protocol {
        Protocol::OpenPlay => Color32::from_rgb(80, 160, 255),
        Protocol::AirPlay => Color32::from_rgb(80, 200, 120),
        Protocol::Miracast => Color32::from_rgb(255, 180, 60),
    };
    (protocol.label(), color)
}

fn receiver_subtitle(receiver: &DiscoveredReceiver) -> String {
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
            if r.model.is_empty() || r.model == "Manual" {
                addr
            } else {
                format!("{} — {addr}:{}", r.model, r.port)
            }
        }
        DiscoveredReceiver::Miracast(r) => match &r.mode {
            MiracastMode::Infrastructure { addr, port } => format!("{addr}:{port}"),
            MiracastMode::WifiDirect { device_address } => format!("P2P {device_address}"),
        },
    }
}
