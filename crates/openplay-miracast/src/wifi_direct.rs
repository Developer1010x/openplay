//! Wi-Fi Direct (P2P) discovery and group formation for Miracast.
//!
//! Uses wpa_supplicant's D-Bus interface to:
//! 1. Discover Miracast sinks via Wi-Fi Direct P2P
//! 2. Form a P2P group with the sink
//! 3. Obtain the IP address for RTSP connection
//!
//! Requires: wpa_supplicant running with D-Bus enabled and P2P support.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// D-Bus service name for wpa_supplicant.
const WPA_SERVICE: &str = "fi.w1.wpa_supplicant1";
/// D-Bus object path for wpa_supplicant.
const WPA_PATH: &str = "/fi/w1/wpa_supplicant1";
/// D-Bus interface for wpa_supplicant.
const WPA_INTERFACE: &str = "fi.w1.wpa_supplicant1";
/// D-Bus interface for P2P device.
const WPA_P2P_INTERFACE: &str = "fi.w1.wpa_supplicant1.Interface.P2PDevice";

/// A discovered Wi-Fi Direct peer (potential Miracast sink).
#[derive(Debug, Clone)]
pub struct WifiDirectPeer {
    /// Peer device name (display name).
    pub name: String,
    /// Peer MAC address / device address (e.g. "AA:BB:CC:DD:EE:FF").
    pub device_address: String,
    /// D-Bus object path for the peer.
    pub object_path: String,
    /// Whether the peer advertises WFD (Wi-Fi Display) support.
    pub wfd_supported: bool,
    /// WFD device info (if available from the peer's WFD IE).
    pub wfd_device_info: Option<u16>,
    /// Peer's primary device type.
    pub device_type: Option<String>,
}

/// Result of a P2P group formation.
#[derive(Debug, Clone)]
pub struct P2PGroupInfo {
    /// The P2P group interface name (e.g., "p2p-wlan0-0").
    pub interface_name: String,
    /// Our IP address in the P2P group.
    pub local_ip: IpAddr,
    /// Peer's IP address in the P2P group.
    pub peer_ip: IpAddr,
    /// Peer's device address.
    pub peer_device_address: String,
}

/// Events from Wi-Fi Direct discovery.
#[derive(Debug, Clone)]
pub enum WifiDirectEvent {
    /// A new Miracast-capable peer was found.
    PeerFound(WifiDirectPeer),
    /// A peer was lost.
    PeerLost { device_address: String },
    /// P2P group formed successfully.
    GroupFormed(P2PGroupInfo),
    /// P2P group removed.
    GroupRemoved,
    /// Error occurred.
    Error(String),
}

/// Wi-Fi Direct P2P manager for Miracast discovery and connection.
pub struct WifiDirectManager {
    _event_tx: mpsc::Sender<WifiDirectEvent>,
    p2p_interface_path: Option<String>,
}

impl WifiDirectManager {
    /// Start Wi-Fi Direct P2P discovery (for the app's background scanner).
    ///
    /// Returns the manager and a receiver for discovery events.
    pub async fn start() -> anyhow::Result<(Self, mpsc::Receiver<WifiDirectEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(64);

        // Find the P2P-capable wireless interface
        let p2p_interface_path = find_p2p_interface().await?;
        info!(interface = %p2p_interface_path, "Found P2P-capable interface");

        // Set WFD IEs so sinks recognize us as a Miracast source
        set_wfd_ies().await;

        let manager = Self {
            _event_tx: event_tx.clone(),
            p2p_interface_path: Some(p2p_interface_path.clone()),
        };

        // Start P2P device discovery
        let tx = event_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = run_p2p_discovery(&p2p_interface_path, tx).await {
                error!(%e, "P2P discovery failed");
            }
        });

        Ok((manager, event_rx))
    }

    /// Start Wi-Fi Direct manager for a connection session.
    ///
    /// Sets up WFD IEs, starts P2P Find scan, and listens for signals.
    /// The Find scan must be active for Connect/GO negotiation to work.
    pub async fn start_for_connect() -> anyhow::Result<(Self, mpsc::Receiver<WifiDirectEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(64);

        let p2p_interface_path = find_p2p_interface().await?;
        info!(interface = %p2p_interface_path, "Found P2P-capable interface for connect");

        // Set WFD IEs
        set_wfd_ies().await;

        let manager = Self {
            _event_tx: event_tx.clone(),
            p2p_interface_path: Some(p2p_interface_path.clone()),
        };

        // Start P2P Find and signal listeners — Find MUST be active for Connect to work
        let tx = event_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = run_p2p_discovery(&p2p_interface_path, tx).await {
                error!(%e, "P2P discovery/signal listener failed");
            }
        });

        // Give the signal listener a moment to register before connect() is called
        tokio::time::sleep(Duration::from_millis(300)).await;

        Ok((manager, event_rx))
    }

    /// Initiate P2P group formation with a specific peer by MAC address.
    ///
    /// NOTE: Do NOT stop P2P Find before Connect — wpa_supplicant needs
    /// the active scan for GO negotiation to work.
    pub async fn connect(&self, peer_mac: &str) -> anyhow::Result<()> {
        let interface_path = self
            .p2p_interface_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No P2P interface available"))?;

        let mac_hex = peer_mac.replace([':', '-'], "").to_lowercase();
        let peer_object_path = format!("{interface_path}/Peers/{mac_hex}");

        info!(peer = %peer_object_path, mac = %peer_mac, "Initiating P2P connection");

        let connection = zbus::Connection::system().await?;
        let proxy = zbus::Proxy::new(
            &connection,
            WPA_SERVICE,
            interface_path.as_str(),
            WPA_P2P_INTERFACE,
        )
        .await?;

        // Remove stale persistent groups from previous connections
        let _ = proxy.call_method("RemoveAllPersistentGroups", &()).await;

        let peer_path = zbus::zvariant::ObjectPath::try_from(peer_object_path.as_str())
            .map_err(|e| anyhow::anyhow!("Invalid peer path '{peer_object_path}': {e}"))?;

        // GO intent = 0: prefer client role (like miraclecast).
        // The sink (display) becomes the GO, we become the client.
        // This is critical — default intent=7 causes GO negotiation failures
        // with many displays that also want to be client.
        let args: HashMap<&str, zbus::zvariant::Value> = HashMap::from([
            ("peer", zbus::zvariant::Value::from(peer_path)),
            ("wps_method", zbus::zvariant::Value::new("pbc")),
            ("go_intent", zbus::zvariant::Value::new(0i32)),
        ]);

        proxy
            .call_method("Connect", &(args,))
            .await
            .map_err(|e| anyhow::anyhow!("P2P connect failed: {e}"))?;

        info!("P2P connection initiated — waiting for GO negotiation");
        Ok(())
    }

    /// Stop P2P discovery and clean up.
    pub async fn stop(&self) -> anyhow::Result<()> {
        if let Some(ref interface_path) = self.p2p_interface_path {
            let connection = zbus::Connection::system().await?;
            let proxy = zbus::Proxy::new(
                &connection,
                WPA_SERVICE,
                interface_path.as_str(),
                WPA_P2P_INTERFACE,
            )
            .await?;

            let _ = proxy.call_method("StopFind", &()).await;
            info!("P2P discovery stopped");
        }
        Ok(())
    }

    /// Disconnect from P2P group.
    pub async fn disconnect(&self) -> anyhow::Result<()> {
        if let Some(ref interface_path) = self.p2p_interface_path {
            let connection = zbus::Connection::system().await?;
            let proxy = zbus::Proxy::new(
                &connection,
                WPA_SERVICE,
                interface_path.as_str(),
                WPA_INTERFACE,
            )
            .await?;

            let _ = proxy.call_method("Disconnect", &()).await;
            info!("P2P group disconnected");
        }
        Ok(())
    }
}

/// Set WFD (Wi-Fi Display) IEs on the global wpa_supplicant interface.
async fn set_wfd_ies() {
    let wfd_ies = build_wfd_ie();
    match zbus::Connection::system().await {
        Ok(connection) => {
            match zbus::Proxy::new(&connection, WPA_SERVICE, WPA_PATH, WPA_INTERFACE).await {
                Ok(root_proxy) => match root_proxy.set_property("WFDIEs", &wfd_ies).await {
                    Ok(()) => info!("WFD IEs set — advertising as WFD source"),
                    Err(e) => warn!(%e, "Failed to set WFD IEs"),
                },
                Err(e) => warn!(%e, "Failed to create wpa proxy for WFD IEs"),
            }
        }
        Err(e) => warn!(%e, "Failed to connect to D-Bus for WFD IEs"),
    }
}

/// Find a P2P-capable wireless interface path (public for session use).
pub async fn find_p2p_interface_path() -> anyhow::Result<String> {
    find_p2p_interface().await
}

/// Resolve Linux ifname from a D-Bus object path (public for session use).
pub async fn resolve_ifname(
    connection: &zbus::Connection,
    obj_path: &str,
) -> anyhow::Result<String> {
    resolve_ifname_from_dbus(connection, obj_path).await
}

/// Find a P2P-capable wireless interface via wpa_supplicant D-Bus.
async fn find_p2p_interface() -> anyhow::Result<String> {
    let connection = zbus::Connection::system().await.map_err(|e| {
        anyhow::anyhow!("Failed to connect to system D-Bus: {e}. Is wpa_supplicant running?")
    })?;

    let proxy = zbus::Proxy::new(&connection, WPA_SERVICE, WPA_PATH, WPA_INTERFACE).await?;

    let interfaces: Vec<zbus::zvariant::OwnedObjectPath> =
        proxy
            .get_property("Interfaces")
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get wpa_supplicant interfaces: {e}"))?;

    if interfaces.is_empty() {
        return Err(anyhow::anyhow!(
            "No wireless interfaces found in wpa_supplicant"
        ));
    }

    // Try to find one that supports P2P
    for iface_path in &interfaces {
        let p2p_proxy = zbus::Proxy::new(
            &connection,
            WPA_SERVICE,
            iface_path.as_str(),
            WPA_P2P_INTERFACE,
        )
        .await?;

        // Try a harmless P2P operation to check if the interface supports it
        match p2p_proxy
            .call_method("Find", &(HashMap::<&str, zbus::zvariant::Value>::new(),))
            .await
        {
            Ok(_) => {
                let _ = p2p_proxy.call_method("StopFind", &()).await;
                return Ok(iface_path.to_string());
            }
            Err(e) => {
                debug!(path = %iface_path, %e, "Interface does not support P2P");
            }
        }
    }

    warn!("No P2P-capable interface found, using first interface");
    Ok(interfaces[0].to_string())
}

/// Run P2P discovery: start Find scan and listen for events.
async fn run_p2p_discovery(
    interface_path: &str,
    event_tx: mpsc::Sender<WifiDirectEvent>,
) -> anyhow::Result<()> {
    let connection = zbus::Connection::system().await?;

    let proxy =
        zbus::Proxy::new(&connection, WPA_SERVICE, interface_path, WPA_P2P_INTERFACE).await?;

    // Start P2P find
    let find_args: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
    proxy
        .call_method("Find", &(find_args,))
        .await
        .map_err(|e| anyhow::anyhow!("P2P Find failed: {e}"))?;

    info!("P2P discovery started, listening for peers...");

    listen_for_signals(&connection, interface_path, event_tx).await
}

/// Shared signal listener for both discovery and connect modes.
async fn listen_for_signals(
    connection: &zbus::Connection,
    interface_path: &str,
    event_tx: mpsc::Sender<WifiDirectEvent>,
) -> anyhow::Result<()> {
    let proxy =
        zbus::Proxy::new(connection, WPA_SERVICE, interface_path, WPA_P2P_INTERFACE).await?;

    let mut device_found = proxy.receive_signal("DeviceFound").await?;
    let mut device_lost = proxy.receive_signal("DeviceLost").await?;
    let mut go_neg_success = proxy.receive_signal("GONegotiationSuccess").await?;
    let mut go_neg_failure = proxy.receive_signal("GONegotiationFailure").await?;
    let mut go_neg_request = proxy.receive_signal("GONegotiationRequest").await?;
    let mut group_started = proxy.receive_signal("GroupStarted").await?;
    let mut group_finished = proxy.receive_signal("GroupFinished").await?;
    let mut group_formation_failure = proxy.receive_signal("GroupFormationFailure").await?;
    let mut prov_disc_pbc_req = proxy.receive_signal("ProvisionDiscoveryPBCRequest").await?;
    let mut prov_disc_pbc_resp = proxy
        .receive_signal("ProvisionDiscoveryPBCResponse")
        .await?;

    loop {
        tokio::select! {
            Some(signal) = StreamExt::next(&mut device_found) => {
                let body: zbus::message::Body = signal.body();
                if let Ok(args) = body.deserialize::<(zbus::zvariant::OwnedObjectPath,)>() {
                    let peer_path = args.0.to_string();
                    match get_peer_info(connection, &peer_path).await {
                        Ok(peer) => {
                            info!(
                                name = %peer.name,
                                addr = %peer.device_address,
                                wfd = peer.wfd_supported,
                                path = %peer.object_path,
                                "Discovered P2P peer"
                            );
                            let _ = event_tx.send(WifiDirectEvent::PeerFound(peer)).await;
                        }
                        Err(e) => {
                            warn!(%e, path = %peer_path, "Failed to get peer info");
                        }
                    }
                }
            }
            Some(signal) = StreamExt::next(&mut device_lost) => {
                let body: zbus::message::Body = signal.body();
                if let Ok(args) = body.deserialize::<(zbus::zvariant::OwnedObjectPath,)>() {
                    let peer_path = args.0.to_string();
                    let device_addr = extract_mac_from_path(&peer_path);
                    let _ = event_tx.send(WifiDirectEvent::PeerLost { device_address: device_addr }).await;
                }
            }
            Some(_signal) = StreamExt::next(&mut go_neg_success) => {
                info!("GO negotiation succeeded!");
            }
            Some(signal) = StreamExt::next(&mut go_neg_failure) => {
                let body: zbus::message::Body = signal.body();
                let details = match body.deserialize::<(HashMap<String, zbus::zvariant::OwnedValue>,)>() {
                    Ok(args) => {
                        let props = args.0;
                        let status = props.get("status")
                            .and_then(|v| i32::try_from(v.clone()).ok())
                            .unwrap_or(-1);
                        format!("status={status}, props={props:?}")
                    }
                    Err(_) => "unknown details".to_string(),
                };
                error!(details = %details, "GO negotiation FAILED");
                let _ = event_tx.send(WifiDirectEvent::Error(
                    format!("GO negotiation failed: {details}")
                )).await;
            }
            Some(signal) = StreamExt::next(&mut go_neg_request) => {
                let body: zbus::message::Body = signal.body();
                let peer = match body.deserialize::<(zbus::zvariant::OwnedObjectPath,)>() {
                    Ok(args) => args.0.to_string(),
                    Err(_) => "unknown".to_string(),
                };
                info!(peer = %peer, "GO negotiation request from peer — auto-accepting");
            }
            Some(signal) = StreamExt::next(&mut group_formation_failure) => {
                let body: zbus::message::Body = signal.body();
                let reason = body.deserialize::<(String,)>()
                    .map(|args| args.0)
                    .unwrap_or_else(|_| "unknown".to_string());
                error!(reason = %reason, "P2P group formation FAILED");
                let _ = event_tx.send(WifiDirectEvent::Error(
                    format!("Group formation failed: {reason}")
                )).await;
            }
            Some(signal) = StreamExt::next(&mut prov_disc_pbc_req) => {
                let body: zbus::message::Body = signal.body();
                let peer = match body.deserialize::<(zbus::zvariant::OwnedObjectPath,)>() {
                    Ok(args) => args.0.to_string(),
                    Err(_) => "unknown".to_string(),
                };
                info!(peer = %peer, "Provision Discovery PBC Request received");
            }
            Some(signal) = StreamExt::next(&mut prov_disc_pbc_resp) => {
                let body: zbus::message::Body = signal.body();
                let peer = match body.deserialize::<(zbus::zvariant::OwnedObjectPath,)>() {
                    Ok(args) => args.0.to_string(),
                    Err(_) => "unknown".to_string(),
                };
                info!(peer = %peer, "Provision Discovery PBC Response received");
            }
            Some(signal) = StreamExt::next(&mut group_started) => {
                info!("P2P group started signal received");
                let body: zbus::message::Body = signal.body();
                if let Ok(args) = body.deserialize::<(HashMap<String, zbus::zvariant::OwnedValue>,)>() {
                    let props = args.0;

                    // Log all properties for debugging
                    for (k, v) in &props {
                        info!(key = %k, value = ?v, "GroupStarted property");
                    }

                    // Get the interface object path (D-Bus path, not Linux ifname)
                    // The value is an ObjectPath, so try ObjectPath extraction first, then String
                    let iface_obj_path = props.get("interface_object")
                        .and_then(|v| {
                            // Try as ObjectPath first (wpa_supplicant sends ObjectPath type)
                            zbus::zvariant::OwnedObjectPath::try_from(v.clone())
                                .map(|p| p.to_string())
                                .ok()
                                .or_else(|| String::try_from(v.clone()).ok())
                        })
                        .or_else(|| props.get("group_object")
                            .and_then(|v| {
                                zbus::zvariant::OwnedObjectPath::try_from(v.clone())
                                    .map(|p| p.to_string())
                                    .ok()
                                    .or_else(|| String::try_from(v.clone()).ok())
                            }))
                        .unwrap_or_default();

                    let role = props.get("role")
                        .and_then(|v| String::try_from(v.clone()).ok())
                        .unwrap_or_else(|| "unknown".to_string());

                    info!(iface_obj = %iface_obj_path, role = %role, "P2P group details");

                    // Resolve the Linux network interface name from the D-Bus object path
                    let ifname = if !iface_obj_path.is_empty() {
                        match resolve_ifname_from_dbus(connection, &iface_obj_path).await {
                            Ok(name) => {
                                info!(ifname = %name, "Resolved P2P group interface name");
                                name
                            }
                            Err(e) => {
                                warn!(%e, "Failed to resolve ifname, using object path");
                                iface_obj_path.clone()
                            }
                        }
                    } else {
                        iface_obj_path.clone()
                    };

                    // Try to extract peer IP from signal properties
                    let peer_ip = props.get("IpAddr")
                        .and_then(|v| <Vec<u8>>::try_from(v.clone()).ok())
                        .filter(|b| b.len() == 4)
                        .map(|b| IpAddr::from([b[0], b[1], b[2], b[3]]))
                        .unwrap_or_else(|| "0.0.0.0".parse().unwrap());

                    let _ = event_tx.send(WifiDirectEvent::GroupFormed(P2PGroupInfo {
                        interface_name: ifname,
                        local_ip: "0.0.0.0".parse().unwrap(),
                        peer_ip,
                        peer_device_address: String::new(),
                    })).await;
                }
            }
            Some(_signal) = StreamExt::next(&mut group_finished) => {
                info!("P2P group finished");
                let _ = event_tx.send(WifiDirectEvent::GroupRemoved).await;
            }
            else => break,
        }
    }

    Ok(())
}

/// Get detailed info about a discovered P2P peer.
async fn get_peer_info(
    connection: &zbus::Connection,
    peer_path: &str,
) -> anyhow::Result<WifiDirectPeer> {
    let proxy = zbus::Proxy::new(
        connection,
        WPA_SERVICE,
        peer_path,
        "fi.w1.wpa_supplicant1.Peer",
    )
    .await?;

    let device_name: String = proxy
        .get_property("DeviceName")
        .await
        .unwrap_or_else(|_| "Unknown".to_string());

    // DeviceAddress is a byte array (ay) in wpa_supplicant D-Bus, not a string
    let device_address = match proxy.get_property::<Vec<u8>>("DeviceAddress").await {
        Ok(bytes) if bytes.len() == 6 => {
            format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
            )
        }
        _ => extract_mac_from_path(peer_path),
    };

    // Check for WFD IEs
    let wfd_ies: Vec<u8> = proxy.get_property("IEs").await.unwrap_or_default();

    let wfd_supported = has_wfd_ie(&wfd_ies);
    let wfd_device_info = parse_wfd_device_info(&wfd_ies);

    Ok(WifiDirectPeer {
        name: device_name,
        device_address,
        object_path: peer_path.to_string(),
        wfd_supported,
        wfd_device_info,
        device_type: None,
    })
}

/// Extract MAC address from a wpa_supplicant peer object path.
/// Handles both formats:
///   /fi/w1/wpa_supplicant1/Interfaces/0/Peers/fa1ccdff7d5f  (hex, no separators)
///   /fi/w1/wpa_supplicant1/Peers/aa_bb_cc_dd_ee_ff           (underscore-separated)
fn extract_mac_from_path(path: &str) -> String {
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('_') {
        // Underscore-separated: aa_bb_cc_dd_ee_ff → aa:bb:cc:dd:ee:ff
        last.replace('_', ":")
    } else if last.len() == 12 && last.chars().all(|c| c.is_ascii_hexdigit()) {
        // Hex-only: fa1ccdff7d5f → fa:1c:cd:ff:7d:5f
        let b = last.as_bytes();
        format!(
            "{}{}:{}{}:{}{}:{}{}:{}{}:{}{}",
            b[0] as char,
            b[1] as char,
            b[2] as char,
            b[3] as char,
            b[4] as char,
            b[5] as char,
            b[6] as char,
            b[7] as char,
            b[8] as char,
            b[9] as char,
            b[10] as char,
            b[11] as char,
        )
    } else {
        last.to_string()
    }
}

/// Resolve the Linux network interface name from a wpa_supplicant D-Bus object path.
async fn resolve_ifname_from_dbus(
    connection: &zbus::Connection,
    interface_obj_path: &str,
) -> anyhow::Result<String> {
    let proxy = zbus::Proxy::new(
        connection,
        WPA_SERVICE,
        interface_obj_path,
        "fi.w1.wpa_supplicant1.Interface",
    )
    .await?;

    let ifname: String = proxy
        .get_property("Ifname")
        .await
        .map_err(|e| anyhow::anyhow!("Failed to get Ifname: {e}"))?;

    Ok(ifname)
}

/// Build WFD (Wi-Fi Display) Information Element for advertising as a WFD source.
fn build_wfd_ie() -> Vec<u8> {
    let mut ie = Vec::new();

    // WFD Subelement: Device Information
    ie.push(0x00); // Subelement ID = 0 (WFD Device Information)
    ie.push(0x00); // Length (high byte)
    ie.push(0x06); // Length (low byte) = 6 bytes

    // WFD Device Information bitmap (matching gnome-network-displays 0x0090):
    // Bits 1-0: Device Type = 00 (Source)
    // Bit 4: Session Available = 1
    // Bit 7: WFD Service Discovery = 1
    let device_info: u16 = 0x0090;
    ie.push((device_info >> 8) as u8);
    ie.push((device_info & 0xFF) as u8);

    // Session Management Control Port = 7236
    ie.push(0x1C); // 7236 >> 8
    ie.push(0x44); // 7236 & 0xFF

    // Maximum Throughput = 200 Mbps (matching gnome-network-displays)
    ie.push(0x00);
    ie.push(0xC8);

    ie
}

/// Check if Wi-Fi P2P IEs contain a WFD Information Element.
fn has_wfd_ie(ies: &[u8]) -> bool {
    if ies.len() >= 6 {
        ies[0] == 0x00 && ies[1] == 0x00 && ies[2] == 0x06
    } else {
        false
    }
}

/// Parse WFD device info from IEs.
fn parse_wfd_device_info(ies: &[u8]) -> Option<u16> {
    if ies.len() >= 5 && ies[0] == 0x00 && ies[2] >= 0x02 {
        Some(((ies[3] as u16) << 8) | ies[4] as u16)
    } else {
        None
    }
}

/// Get the IP address of a P2P peer after group formation.
pub async fn resolve_peer_ip(interface_name: &str, timeout: Duration) -> anyhow::Result<IpAddr> {
    let start = tokio::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for peer IP on {interface_name}"
            ));
        }

        match get_peer_ip_from_arp(interface_name).await {
            Ok(ip) => return Ok(ip),
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Look up peer IP from ARP table for a given interface.
async fn get_peer_ip_from_arp(interface_name: &str) -> anyhow::Result<IpAddr> {
    let output = tokio::process::Command::new("ip")
        .args(["neigh", "show", "dev", interface_name])
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 && parts.iter().any(|&p| p == "REACHABLE" || p == "STALE") {
            if let Ok(ip) = parts[0].parse::<IpAddr>() {
                return Ok(ip);
            }
        }
    }

    Err(anyhow::anyhow!(
        "No reachable peer found on {interface_name}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_wfd_ie() {
        let ie = build_wfd_ie();
        assert_eq!(ie[0], 0x00); // Subelement ID
        assert_eq!(ie[2], 0x06); // Length
                                 // Device info: 0x0090 (Source, Session Available, WFD Service Discovery)
        let device_info = ((ie[3] as u16) << 8) | ie[4] as u16;
        assert_eq!(device_info, 0x0090);
        let port = ((ie[5] as u16) << 8) | ie[6] as u16;
        assert_eq!(port, 7236);
    }

    #[test]
    fn test_has_wfd_ie() {
        let wfd_ie = vec![0x00, 0x00, 0x06, 0x00, 0x90, 0x1C, 0x44, 0x00, 0xC8];
        assert!(has_wfd_ie(&wfd_ie));
        assert!(!has_wfd_ie(&[]));
    }

    #[test]
    fn test_extract_mac_from_path() {
        // Underscore-separated format
        let path = "/fi/w1/wpa_supplicant1/Peers/aa_bb_cc_dd_ee_ff";
        assert_eq!(extract_mac_from_path(path), "aa:bb:cc:dd:ee:ff");

        // Hex-only format (actual wpa_supplicant format)
        let path2 = "/fi/w1/wpa_supplicant1/Interfaces/0/Peers/fa1ccdff7d5f";
        assert_eq!(extract_mac_from_path(path2), "fa:1c:cd:ff:7d:5f");
    }
}
