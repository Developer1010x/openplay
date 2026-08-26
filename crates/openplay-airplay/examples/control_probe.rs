//! Probes what a receiver exposes *after* a transient pair-setup.
//!
//! `pair_probe` stops once SRP completes. This goes one step further: it opens
//! the encrypted control channel with the negotiated session key and asks the
//! receiver which endpoints it actually serves. Both halves are things no unit
//! test can answer, because both depend on the receiver's behaviour.
//!
//! ```console
//! cargo run -p openplay-airplay --example control_probe -- 192.168.1.11:7000
//! ```

use openplay_airplay::{control_channel::ControlChannel, hap_pairing, http_session};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let addr: std::net::SocketAddr = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: control_probe <ip>:<port>"))?
        .parse()?;

    println!("── transient pair-setup against {addr}");
    let session = hap_pairing::pair_setup_transient(addr).await?;
    println!(
        "   ✅ M4 verified, session key {} bytes",
        session.session_key.len()
    );

    println!("\n── encrypted control channel");
    let mut chan = ControlChannel::new(session.stream, &session.session_key)?;
    let info = b"GET /info HTTP/1.1\r\nUser-Agent: AirPlay/540.31\r\nContent-Length: 0\r\n\r\n";
    let reply = chan.request(info).await?;
    let head = String::from_utf8_lossy(&reply);
    println!(
        "   ✅ {} ({} bytes decrypted)",
        head.lines().next().unwrap_or("<none>"),
        reply.len()
    );

    println!("\n── endpoints, over the encrypted channel");
    let probes: Vec<(&str, Vec<u8>)> = vec![
        (
            "POST /stream",
            http_session::build_stream_request(1920, 1080, 30, "probe-session")?,
        ),
        (
            "POST /fp-setup",
            b"POST /fp-setup HTTP/1.1\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ),
        (
            "SETUP (RTSP)",
            b"SETUP rtsp://x/stream RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ),
        (
            "GET /server-info",
            b"GET /server-info HTTP/1.1\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ),
    ];
    for (name, req) in probes {
        match chan.request(&req).await {
            Ok(r) => {
                let t = String::from_utf8_lossy(&r);
                println!("   {:<18} {}", name, t.lines().next().unwrap_or("<none>"));
            }
            Err(e) => {
                println!("   {name:<18} ❌ {e}");
                break;
            }
        }
    }

    println!("\n   Reading the result:");
    println!("   • /stream 404          — no legacy AirPlay 1 mirroring endpoint");
    println!("   • /fp-setup 400 not 404 — FairPlay endpoint exists, wants a real body");
    println!("   • SETUP 455            — RTSP works, but needs prior state (fp-setup)");
    println!("   FairPlay is not implemented here by decision; see docs/crypto.md.");
    Ok(())
}
