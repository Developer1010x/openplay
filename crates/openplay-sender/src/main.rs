mod app;
mod casting;
mod receiver_list;
mod window;

use clap::Parser;
use tracing::info;

/// OpenPlay Sender — Cast your screen to any OpenPlay, AirPlay, or Miracast receiver.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Override config file path.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Override display name.
    #[arg(long)]
    name: Option<String>,
}

fn main() -> anyhow::Result<()> {
    openplay_common::init_logging();
    let args = Args::parse();

    info!("OpenPlay Sender starting");

    // Initialize GStreamer early (required before any pipeline operations)
    openplay_pipeline::init()?;
    info!("GStreamer initialized");

    let mut config = match &args.config {
        Some(path) => openplay_common::AppConfig::load_from(path)?,
        None => openplay_common::AppConfig::load()?,
    };

    if let Some(name) = &args.name {
        config.display_name = name.clone();
    }

    openplay_common::ensure_dirs()?;

    let exit_code = app::run(config);
    std::process::exit(exit_code);
}
