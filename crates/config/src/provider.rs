use std::path::PathBuf;

use serde::Serialize;

/// CLI arguments that can be merged into the config via figment.
///
/// Only non-`None` fields are serialized, so they act as overrides
/// rather than resetting values to `None`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CliArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
}
