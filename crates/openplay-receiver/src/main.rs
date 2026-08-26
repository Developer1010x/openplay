mod app;
mod window;

use clap::Parser;
use tracing::{info, warn};

/// OpenPlay Receiver — Display incoming screen casts.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Override config file path.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Override display name (shown in the receiver window).
    ///
    /// This does *not* affect discovery: the receiver does not advertise
    /// itself over mDNS yet, so no sender can find it by name.
    #[arg(long)]
    name: Option<String>,

    /// Override signaling port. Reserved — has no effect yet.
    ///
    /// The receiver does not open a socket, so nothing binds this port. It is
    /// accepted and validated so the flag keeps working once the signaling
    /// server is wired up.
    #[arg(long)]
    port: Option<u16>,
}

fn main() -> anyhow::Result<()> {
    openplay_common::init_logging();
    let args = Args::parse();

    info!("OpenPlay Receiver starting");

    let mut config = match &args.config {
        Some(path) => openplay_common::AppConfig::load_or_create_at(path)?,
        None => openplay_common::AppConfig::load_or_create()?,
    };

    if let Some(name) = &args.name {
        config.display_name = name.clone();
    }
    if let Some(port) = args.port {
        // Validated and stored, but nothing binds it: the receiver has no
        // signaling server yet. Say so rather than letting the flag imply the
        // receiver is reachable on that port.
        warn!(
            port,
            "--port has no effect yet: this receiver does not listen for connections"
        );
        config.port = port;
    }

    // Re-validate after the CLI overrides: `--port 0` and `--name ""` are just
    // as invalid as the same values in the file, and only this check sees them.
    config.validate()?;

    openplay_common::ensure_dirs()?;

    app::run(config)
}
