//! Drives the real XDG Desktop Portal twice, to show that the screen choice
//! survives between sessions.
//!
//! The first session shows the desktop's screen-selection dialog (unless a
//! choice is already remembered). The second must not: it hands back the token
//! the first one saved. No unit test can check this, because it depends on the
//! portal backend the desktop actually runs.
//!
//! ```console
//! cargo run -p openplay-capture --example portal_probe            # two sessions
//! cargo run -p openplay-capture --example portal_probe -- --forget  # picker again first
//! ```
//!
//! Linux only. On other platforms it just says so.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    use openplay_capture::{forget_screen_choice, CaptureSession};

    if std::env::args().any(|a| a == "--forget") {
        match forget_screen_choice() {
            Ok(true) => println!("── forgot the remembered screen"),
            Ok(false) => println!("── nothing was remembered"),
            Err(e) => println!("── could not forget: {e}"),
        }
    }

    for attempt in 1..=2 {
        println!("── session {attempt} (the picker should appear at most once)");
        match CaptureSession::start().await {
            Ok(session) => {
                let src = session.primary_source().expect("portal returned a stream");
                println!(
                    "   ✅ node {} {}x{}",
                    src.node_id,
                    src.width.unwrap_or(0),
                    src.height.unwrap_or(0)
                );
            }
            Err(e) => {
                println!("   ❌ {e}");
                std::process::exit(1);
            }
        }
    }
    println!("── done: two sessions, one dialog at most");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("portal_probe is Linux-only: there is no portal to drive here.");
}
