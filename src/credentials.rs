//! The stored API key: one account, in an owner-only file.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{CliError, Result};

/// Contents of `credentials.json`. The password is never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credentials {
    pub base_url: String,
    pub email: String,
    pub token_name: String,
    pub key: String,
}

/// `~/.config/sdm/credentials.json` (or the platform equivalent).
///
/// # Errors
/// Returns a usage error when the platform has no config directory.
pub fn path() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join("sdm").join("credentials.json"))
        .ok_or_else(|| CliError::usage("cannot locate a config directory for credentials"))
}

/// Reads the stored credentials; `None` when not logged in.
#[must_use]
pub fn load() -> Option<Credentials> {
    let text = std::fs::read_to_string(path().ok()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes the credentials file with mode 0600.
///
/// # Errors
/// Returns a usage error when the file cannot be written.
pub fn save(credentials: &Credentials) -> Result<PathBuf> {
    let path = path()?;
    let io_err =
        |err: std::io::Error| CliError::usage(format!("cannot write {}: {err}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io_err)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(io_err)?;
    #[cfg(unix)]
    {
        // `mode` only applies on creation; tighten a pre-existing file too.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(io_err)?;
    }
    let text = serde_json::to_string_pretty(credentials).expect("credentials serialize");
    file.write_all(text.as_bytes()).map_err(io_err)?;
    file.write_all(b"\n").map_err(io_err)?;
    Ok(path)
}

/// Deletes the credentials file. Returns whether a file was removed.
///
/// # Errors
/// Returns a usage error when the file exists but cannot be removed.
pub fn delete() -> Result<bool> {
    let path = path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(CliError::usage(format!(
            "cannot remove {}: {err}",
            path.display()
        ))),
    }
}
