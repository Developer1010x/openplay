mod app;
mod casting;
mod receiver_list;

use clap::Parser;
use tracing::info;

/// OpenPlay — Cast your screen to any AirPlay, Miracast, or OpenPlay receiver.
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

    info!("OpenPlay starting");

    openplay_pipeline::init()?;
    info!("GStreamer initialized");

    let mut config = match &args.config {
        Some(path) => openplay_common::AppConfig::load_or_create_at(path)?,
        None => openplay_common::AppConfig::load_or_create()?,
    };

    if let Some(name) = &args.name {
        config.display_name = name.clone();
    }

    // Re-validate after the CLI overrides: `--name ""` is just as invalid as an
    // empty display_name in the file, and only this check sees it.
    config.validate()?;

    openplay_common::ensure_dirs()?;

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpenPlay")
            .with_inner_size([440.0, 580.0])
            .with_min_inner_size([360.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "OpenPlay",
        native_options,
        Box::new(|cc| Ok(Box::new(app::SenderApp::new(cc, config)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI error: {e}"))
}
