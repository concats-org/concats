pub mod model;
pub mod provider;

use std::{io::Write, path::PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
pub use model::{AgentConfig, AppConfig, Config, SyncConfig};
pub use provider::CliArgs as ConfigCliArgs;

use crate::provider::CliArgs;

/// Load configuration with figment layering:
///
/// 1. Built-in defaults
/// 2. TOML config file (`~/.config/concats/config.toml`)
/// 3. Environment variables (`CONCATS_*`; app settings use `CONCATS_APP__*`)
/// 4. CLI arguments (final overrides)
///
/// # Errors
///
/// Returns an error if the layered configuration cannot be deserialized into
/// [`Config`].
pub fn load_config(cli: &CliArgs) -> miette::Result<Config> {
    let config_path = config_dir()
        .ok_or_else(|| miette::miette!("no platform configuration directory"))?
        .join("config.toml");

    let figment = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(&config_path))
        .merge(
            Env::prefixed("CONCATS_")
                .filter(|key| !key.starts_with("APP_"))
                .split("_"),
        )
        .merge(Env::prefixed("CONCATS_APP__").map(|key| format!("app.{}", key.as_str()).into()))
        .merge(Serialized::defaults(cli));

    figment
        .extract()
        .map_err(|e| miette::miette!("config error: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::result_large_err,
        reason = "Figment fixes the test-jail callback error type"
    )]
    fn app_settings_share_the_file_but_ignore_terminal_and_test_variables() {
        figment::Jail::expect_with(|jail| {
            let dir = jail.directory().to_string_lossy().into_owned();
            jail.set_env("HOME", &dir);
            jail.set_env("XDG_CONFIG_HOME", &dir);
            jail.set_env("CONCATS_APP_REPO", "/a/repository");
            jail.set_env("CONCATS_APP_SHOT", "/a/screenshot.png");
            jail.set_env("CONCATS_APP__FONT_SIZE", 12.0);
            let mut config = Config::default();
            config.app.theme = "Saved theme".into();
            config.app.font_size = 10.0;
            config.workspace = Some("saved-workspace".into());
            save_config(&config).unwrap();

            let loaded = load_config(&CliArgs {
                workspace: Some("cli-workspace".into()),
                ..CliArgs::default()
            })
            .unwrap();
            assert_eq!(loaded.app.theme, "Saved theme");
            assert!((loaded.app.font_size - 12.0).abs() < f64::EPSILON);
            assert_eq!(loaded.workspace, Some("cli-workspace".into()));
            let persisted: Config = toml::from_str(
                &std::fs::read_to_string(config_dir().unwrap().join("config.toml")).unwrap(),
            )
            .unwrap();
            assert_eq!(persisted.workspace, Some("saved-workspace".into()));
            Ok(())
        });
    }
}
