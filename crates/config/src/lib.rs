pub mod model;

use std::{io::Write, path::PathBuf};

pub use model::{AppConfig, Config, SyncConfig};

/// Load configuration from the TOML config file
/// (`~/.config/concats/config.toml`). What the file leaves out, or a missing
/// file, takes the built-in default.
///
/// # Errors
///
/// Returns an error if the file cannot be read or does not deserialize into
/// [`Config`].
pub fn load_config() -> miette::Result<Config> {
    let config_path = config_dir()
        .ok_or_else(|| miette::miette!("no platform configuration directory"))?
        .join("config.toml");
    let text = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => {
            return Err(miette::miette!(
                "cannot read {}: {error}",
                config_path.display()
            ));
        }
    };
    toml::from_str(&text).map_err(|e| miette::miette!("config error: {e}"))
}

/// Save configuration to the TOML config file.
///
/// # Errors
///
/// Returns an error if the configuration directory cannot be created, the
/// config cannot be serialized to TOML, or the config file cannot be written.
pub fn save_config(config: &Config) -> miette::Result<()> {
    let dir = config_dir().ok_or_else(|| miette::miette!("no platform configuration directory"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| miette::miette!("failed to create config dir: {e}"))?;

    let path = dir.join("config.toml");
    let toml_str = toml::to_string_pretty(config)
        .map_err(|e| miette::miette!("failed to serialize config: {e}"))?;

    let mut pending = tempfile::NamedTempFile::new_in(&dir)
        .map_err(|e| miette::miette!("failed to create config file: {e}"))?;
    pending
        .write_all(toml_str.as_bytes())
        .and_then(|()| pending.as_file().sync_all())
        .map_err(|e| miette::miette!("failed to write config file: {e}"))?;
    pending
        .persist(&path)
        .map_err(|e| miette::miette!("failed to replace config file: {e}"))?;

    tracing::info!("saved config to {}", path.display());
    Ok(())
}

/// Return the configuration directory (`~/.config/concats`).
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("concats"))
}
