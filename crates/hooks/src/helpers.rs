use std::path::PathBuf;

use concats_core::error::Result;

/// Return the path to the current `concats` executable.
///
/// # Errors
///
/// Returns an error if the current executable path cannot be determined.
pub fn binary_path() -> Result<PathBuf> {
    Ok(std::env::current_exe()?)
}
