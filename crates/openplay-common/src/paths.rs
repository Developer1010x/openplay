use directories::ProjectDirs;
use std::path::PathBuf;

const QUALIFIER: &str = "org";
const ORG: &str = "openplay";
const APP: &str = "OpenPlay";

fn project_dirs() -> ProjectDirs {
    ProjectDirs::from(QUALIFIER, ORG, APP).expect("Failed to determine XDG directories")
}

/// Returns `$XDG_CONFIG_HOME/openplay/` (e.g. `~/.config/openplay/`).
pub fn config_dir() -> PathBuf {
    project_dirs().config_dir().to_path_buf()
}

/// Returns `$XDG_DATA_HOME/openplay/` (e.g. `~/.local/share/openplay/`).
pub fn data_dir() -> PathBuf {
    project_dirs().data_dir().to_path_buf()
}

/// Creates config and data directories if they don't exist.
pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::create_dir_all(data_dir())?;
    Ok(())
}
