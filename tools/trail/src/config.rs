//! Local secrets and settings, all outside the repository: ~/.config/cairn
//! (0600 files) and ~/cairn-backups.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

pub fn dir() -> PathBuf {
    home().join(".config/cairn")
}

/// KEY=VALUE lines from ~/.config/cairn/<name>, refusing a file others can read.
pub fn load_env(name: &str) -> Result<HashMap<String, String>> {
    let path = dir().join(name);
    let meta = std::fs::metadata(&path).with_context(|| format!("missing {}", path.display()))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{} is readable by others (mode {mode:o}); chmod 600 it",
            path.display()
        );
    }
    Ok(std::fs::read_to_string(&path)?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect())
}

/// Write a file only its owner can read, creating its directory 0700.
pub fn save_private(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}
