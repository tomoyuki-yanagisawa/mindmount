//! Resolution and preparation of the local Mindmount data directory.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub const CONFIG_DIR_NAME: &str = "mindmount";
pub const CONFIG_FILE_NAME: &str = "config.toml";
const DATABASE_FILE_NAME: &str = "mindmount.sqlite";

/// Resolves the local Mindmount data directory in one place.
///
/// `XDG_CONFIG_HOME` is preferred over `~/.config`. The resolution is kept
/// here so a future move to `XDG_DATA_HOME` only touches this type.
#[derive(Debug, Clone)]
pub struct AppPaths {
    config_dir: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> Result<Self> {
        let base = match env::var_os("XDG_CONFIG_HOME") {
            Some(value) if !value.is_empty() => {
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    bail!(
                        "XDG_CONFIG_HOME must be an absolute path, got {}",
                        path.display()
                    );
                }
                path
            }
            _ => {
                let home = env::var_os("HOME").context(
                    "neither XDG_CONFIG_HOME nor HOME is set; cannot locate the config directory",
                )?;
                PathBuf::from(home).join(".config")
            }
        };

        Ok(Self::new(base.join(CONFIG_DIR_NAME)))
    }

    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }

    pub fn database_file(&self) -> PathBuf {
        self.config_dir.join("db").join(DATABASE_FILE_NAME)
    }

    pub fn id_state_file(&self) -> PathBuf {
        self.config_dir.join("id-state")
    }

    /// Holds an advisory lock for the duration of a CLI operation.
    pub fn lock(&self) -> Result<fs::File> {
        let path = self.config_dir.join("mindmount.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("cannot open lock file {}", path.display()))?;
        file.try_lock().with_context(|| {
            format!(
                "cannot lock {}; another Mindmount process may be running",
                path.display()
            )
        })?;
        Ok(file)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        ensure_private_dir(&self.config_dir)?;
        ensure_private_dir(&self.config_dir.join("db"))
    }
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("cannot create directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::metadata(path)?.permissions();
        let restricted = permissions.mode() & !0o077;
        if restricted != permissions.mode() {
            fs::set_permissions(path, fs::Permissions::from_mode(restricted))
                .with_context(|| format!("cannot restrict permissions on {}", path.display()))?;
        }
    }
    Ok(())
}
