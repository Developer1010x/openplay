mod app;
mod net;
mod window;

use clap::Parser;
use tracing::info;

/// OpenPlay Receiver — Display incoming screen casts.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Override config file path.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Override display name (shown in the receiver window).
    ///
    /// This is the name senders see: it is published as the mDNS `dn` TXT key
    /// and shown in the approval prompt on this screen.
    #[arg(long)]
    name: Option<String>,

    /// Override the signaling port that senders connect to.
    ///
    /// Also published over mDNS, so a sender discovers the override rather than
    /// the default.
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
        config.port = port;
    }

    // Re-validate after the CLI overrides: `--port 0` and `--name ""` are just
    // as invalid as the same values in the file, and only this check sees them.
    config.validate()?;

    openplay_common::ensure_dirs()?;

    // GStreamer must be initialised before any pipeline is constructed, and the
    // receiver builds one the moment a sender's offer arrives.
    openplay_pipeline::init().map_err(|e| anyhow::anyhow!("GStreamer init failed: {e}"))?;

    app::run(config)
}
