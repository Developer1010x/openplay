//! Probes a real AirPlay receiver: feature negotiation, then HAP pair-setup.
//!
//! This is the hardware check that no unit test in this repository can perform.
//! It exists to answer the open question in issue #27 — whether the corrected
//! RFC 5054 SRP group actually lets `pair_setup` succeed against real Apple
//! hardware, rather than merely removing a known blocker.
//!
//! ```console
//! cargo run -p openplay-airplay --example pair_probe -- 192.168.1.11:7000
//! cargo run -p openplay-airplay --example pair_probe -- 192.168.1.11:7000 1234
//! ```
//!
//! With no PIN argument it attempts transient pairing (the "Everyone on the
//! Same Network" mode). With a PIN it runs the full PIN flow, which raises a
//! dialog on the receiver.

use std::net::SocketAddr;
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let addr: SocketAddr = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: pair_probe <ip:port> [pin]"))?
        .parse()?;
    let pin = args.next();

    println!("── Probing AirPlay receiver at {addr}\n");

    // Step 1: what does it advertise?
    println!("── GET /info");
    let session_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    match TcpStream::connect(addr).await {
        Ok(mut stream) => {
            match openplay_airplay::http_session::get_info_raw(&mut stream, &session_id).await {
                Ok((headers, body)) => {
                    let status = headers.lines().next().unwrap_or("(no status line)");
                    println!("   status:    {status}");
                    println!("   body:      {} bytes", body.len());
                    match openplay_airplay::http_session::parse_info_response_pub(&body) {
                        Ok(info) => {
                            println!("   name:      {}", info.device_name);
                            println!("   model:     {}", info.model);
                            println!("   version:   {}", info.source_version);
                            println!("   features:  0x{:016X}", info.features.raw());
                            println!(
                                "     mirroring:          {}",
                                info.features.supports_mirroring()
                            );
                            println!(
                                "     video:              {}",
                                info.features.supports_video()
                            );
                            println!(
                                "     audio:              {}",
                                info.features.supports_audio()
                            );
                            println!(
                                "     HK pairing required:{}",
                                info.features.requires_hk_pairing()
                            );
                            println!(
                                "     transient pairing:  {}",
                                info.features.supports_transient_pairing()
                            );
                        }
                        Err(e) => println!("   parse failed: {e}"),
                    }
                }
                Err(e) => println!("   FAILED: {e}"),
            }
        }
        Err(e) => println!("   connect FAILED: {e}"),
    }

    // Step 2: the actual question — does SRP-6a agree with real hardware?
    println!("\n── HAP pair-setup");
    // The two modes end in different places, so they cannot share a result type.
    // PIN pairing runs M1-M6 and yields long-term identities; transient stops at
    // M4 with only a session key, because there is no identity exchange.
    let result = match &pin {
        Some(p) => {
            println!("   mode: PIN ({p}) — expect a dialog on the receiver\n");
            openplay_airplay::hap_pairing::pair_setup(addr, p)
                .await
                .map(|r| {
                    format!(
                        "accessory id:   {}\n   accessory LTPK: {}\n   client LTPK:    {}",
                        r.accessory_id,
                        hex(&r.accessory_ltpk),
                        hex(&r.client_ltpk)
                    )
                })
        }
        None => {
            println!("   mode: transient (no PIN)\n");
            openplay_airplay::hap_pairing::pair_setup_transient(addr)
                .await
                .map(|s| {
                    format!(
                        "session key:    {} bytes (no long-term identity)",
                        s.session_key.len()
                    )
                })
        }
    };

    match result {
        Ok(detail) => {
            println!("\n✅ PAIR-SETUP SUCCEEDED");
            println!("   {detail}");
            println!("\n   The SRP group is correct and the receiver accepted our M3 proof.");
        }
        Err(e) => {
            println!("\n❌ PAIR-SETUP FAILED: {e}");
            println!("\n   Where it failed is what matters for issue #27:");
            println!("   - 'Server proof verification failed' (M4) → SRP still disagrees");
            println!("   - missing salt/public key (M2)            → receiver refused earlier");
            println!("   - TLV8 error code in M4                   → receiver rejected the PIN");
            println!("   - connection refused/reset                → not a crypto problem");
            std::process::exit(1);
        }
    }

    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
