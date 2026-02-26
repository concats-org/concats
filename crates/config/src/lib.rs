pub mod model;
pub mod provider;

use std::path::PathBuf;

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};

use crate::provider::CliArgs;

pub use model::{AgentConfig, Config};
pub use provider::CliArgs as ConfigCliArgs;

/// Load configuration with figment layering:
///
/// 1. Built-in defaults
/// 2. TOML config file (`~/.config/concats/config.toml`)
/// 3. Environment variables (`CONCATS_*`)
/// 4. CLI arguments (final overrides)
pub fn load_config(cli: &CliArgs) -> miette::Result<Config> {
    let config_path = config_dir().join("config.toml");

    let figment = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(&config_path))
        .merge(Env::prefixed("CONCATS_").split("_"))
        .merge(Serialized::defaults(cli));

    figment
        .extract()
        .map_err(|e| miette::miette!("config error: {e}"))
}

/// Save configuration to the TOML config file.
pub fn save_config(config: &Config) -> miette::Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| miette::miette!("failed to create config dir: {e}"))?;

    let path = dir.join("config.toml");
    let toml_str = toml::to_string_pretty(config)
        .map_err(|e| miette::miette!("failed to serialize config: {e}"))?;

    std::fs::write(&path, toml_str)
        .map_err(|e| miette::miette!("failed to write config file: {e}"))?;

    tracing::info!("saved config to {}", path.display());
    Ok(())
}

/// Return the configuration directory (`~/.config/concats`).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("concats")
}
