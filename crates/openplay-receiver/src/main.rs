mod app;
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

    /// Override display name (shown in mDNS discovery).
    #[arg(long)]
    name: Option<String>,

    /// Override signaling port.
    #[arg(long)]
    port: Option<u16>,
}

fn main() -> anyhow::Result<()> {
    openplay_common::init_logging();
    let args = Args::parse();

    info!("OpenPlay Receiver starting");

    let mut config = match &args.config {
        Some(path) => openplay_common::AppConfig::load_from(path)?,
        None => openplay_common::AppConfig::load()?,
    };

    if let Some(name) = &args.name {
        config.display_name = name.clone();
    }
    if let Some(port) = args.port {
        config.port = port;
    }

    openplay_common::ensure_dirs()?;

    app::run(config)
}
