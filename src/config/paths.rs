//! Resolution and preparation of the local Mindmount directories.
//!
//! Configuration, persistent data, runtime state, and cache are separated
//! following the XDG base directory specification on macOS and Linux.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use etcetera::{choose_app_strategy, AppStrategy, AppStrategyArgs};

pub const CONFIG_DIR_NAME: &str = "mindmount";
pub const CONFIG_FILE_NAME: &str = "config.toml";
const DATABASE_FILE_NAME: &str = "mindmount.sqlite";

/// Resolves the local Mindmount directories in one place.
///
/// Configuration lives in `XDG_CONFIG_HOME`, persistent data (the database
/// and id state) in `XDG_DATA_HOME`, runtime files (pid, endpoint, log) in
/// `XDG_STATE_HOME`, and caches in `XDG_CACHE_HOME`.
#[derive(Debug, Clone)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> Result<Self> {
        let strategy = choose_app_strategy(AppStrategyArgs {
            top_level_domain: "dev".to_string(),
            author: "Mindmount".to_string(),
            app_name: CONFIG_DIR_NAME.to_string(),
        })
        .context("cannot resolve the XDG directories")?;

        Ok(Self {
            config_dir: strategy.config_dir().to_path_buf(),
            data_dir: strategy.data_dir().to_path_buf(),
            state_dir: strategy.state_dir().unwrap_or_else(|| strategy.data_dir()),
            cache_dir: strategy.cache_dir().to_path_buf(),
        })
    }

    /// Uses a single directory for every category. Primarily for tests.
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        let config_dir = config_dir.into();
        Self {
            data_dir: config_dir.clone(),
            state_dir: config_dir.clone(),
            cache_dir: config_dir.join("cache"),
            config_dir,
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join("credentials.toml")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE_NAME)
    }

    pub fn id_state_file(&self) -> PathBuf {
        self.data_dir.join("id-state")
    }

    /// Prevents two daemons from running at the same time.
    pub fn daemon_lock(&self) -> Result<fs::File> {
        acquire_lock(&self.state_dir.join("mindmount.daemon.lock"))
    }

    pub fn daemon_pid_file(&self) -> PathBuf {
        self.state_dir.join("daemon.pid")
    }

    /// The URL the running daemon bound to. CLI clients read this file so a
    /// non-default port does not need to be repeated on every command.
    pub fn daemon_endpoint_file(&self) -> PathBuf {
        self.state_dir.join("daemon.endpoint")
    }

    pub fn daemon_log_file(&self) -> PathBuf {
        self.state_dir.join("mindmount.log")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        ensure_private_dir(&self.config_dir)?;
        ensure_private_dir(&self.data_dir)?;
        ensure_private_dir(&self.state_dir)?;
        ensure_private_dir(&self.cache_dir)?;
        self.migrate_legacy_layout()
    }

    /// Moves the pre-XDG database and id state out of the config directory.
    fn migrate_legacy_layout(&self) -> Result<()> {
        let legacy_db = self.config_dir.join("db").join(DATABASE_FILE_NAME);
        if legacy_db.is_file() && !self.database_file().exists() {
            fs::rename(&legacy_db, self.database_file()).with_context(|| {
                format!(
                    "cannot move the database from {} to {}",
                    legacy_db.display(),
                    self.database_file().display()
                )
            })?;
            for suffix in ["-wal", "-shm"] {
                let source = PathBuf::from(format!("{}{suffix}", legacy_db.display()));
                if source.exists() {
                    let target =
                        PathBuf::from(format!("{}{suffix}", self.database_file().display()));
                    let _ = fs::rename(source, target);
                }
            }
        }

        let legacy_ids = self.config_dir.join("id-state");
        if legacy_ids.is_file() && !self.id_state_file().exists() {
            fs::rename(&legacy_ids, self.id_state_file()).with_context(|| {
                format!(
                    "cannot move the id state from {} to {}",
                    legacy_ids.display(),
                    self.id_state_file().display()
                )
            })?;
        }
        Ok(())
    }
}

fn acquire_lock(path: &Path) -> Result<fs::File> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("cannot open lock file {}", path.display()))?;
    file.try_lock().with_context(|| {
        format!(
            "cannot lock {}; another Mindmount process may be running",
            path.display()
        )
    })?;
    Ok(file)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn moves_legacy_database_and_id_state_into_the_data_directory() {
        let temp = tempdir().unwrap();
        let config_dir = temp.path().join("config/mindmount");
        fs::create_dir_all(config_dir.join("db")).unwrap();
        fs::write(config_dir.join("db/mindmount.sqlite"), "database").unwrap();
        fs::write(config_dir.join("id-state"), "ids").unwrap();

        let paths = AppPaths {
            data_dir: temp.path().join("data/mindmount"),
            state_dir: temp.path().join("state/mindmount"),
            cache_dir: temp.path().join("cache/mindmount"),
            config_dir: config_dir.clone(),
        };
        paths.ensure_dirs().unwrap();

        assert_eq!(
            fs::read_to_string(paths.database_file()).unwrap(),
            "database"
        );
        assert_eq!(fs::read_to_string(paths.id_state_file()).unwrap(), "ids");
        assert!(!config_dir.join("db/mindmount.sqlite").exists());
        assert!(!config_dir.join("id-state").exists());
    }
}
