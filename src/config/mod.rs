//! `config.toml`: the authoritative mount registry.

pub mod credentials;
pub mod paths;

pub use credentials::Credentials;
pub use paths::AppPaths;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const MAX_NODE_ID: u16 = 1023;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Persisted Snowflake node id (10 bits).
    pub node_id: u16,
    #[serde(default)]
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mount {
    pub id: i64,
    pub name: String,
    pub project_path: PathBuf,
    pub directory_path: PathBuf,
    pub created_at: DateTime<Utc>,
}

impl Config {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.is_file() {
            return Self::load(path);
        }
        if path.exists() {
            bail!("config path exists but is not a file: {}", path.display());
        }

        let config = Self {
            node_id: generate_node_id(),
            mounts: Vec::new(),
        };
        config.save(path)?;
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("cannot parse config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("cannot serialize config")?;
        write_atomic(path, text.as_bytes())
            .with_context(|| format!("cannot write config {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.node_id > MAX_NODE_ID {
            bail!(
                "config node_id must be between 0 and {}, got {}",
                MAX_NODE_ID,
                self.node_id
            );
        }

        for (index, mount) in self.mounts.iter().enumerate() {
            if mount.id <= 0 {
                bail!("mount id must be positive, got {}", mount.id);
            }
            if mount.name.trim().is_empty() {
                bail!("mount {} has an empty name", mount.id);
            }
            if !mount.project_path.is_absolute() || !mount.directory_path.is_absolute() {
                bail!("mount {} paths must be absolute", mount.id);
            }
            if !mount.directory_path.starts_with(&mount.project_path) {
                bail!(
                    "mount {} directory {} is outside project {}",
                    mount.id,
                    mount.directory_path.display(),
                    mount.project_path.display()
                );
            }

            for other in &self.mounts[..index] {
                if mount.directory_path == other.directory_path {
                    bail!(
                        "config registers the same directory twice: {}",
                        mount.directory_path.display()
                    );
                }
                if mount.directory_path.starts_with(&other.directory_path)
                    || other.directory_path.starts_with(&mount.directory_path)
                {
                    bail!(
                        "config has nested mounts: {} and {}",
                        other.directory_path.display(),
                        mount.directory_path.display()
                    );
                }
            }
        }

        Ok(())
    }

    pub fn find_by_id(&self, id: i64) -> Option<&Mount> {
        self.mounts.iter().find(|mount| mount.id == id)
    }

    pub fn find_by_directory(&self, directory: &Path) -> Option<&Mount> {
        self.mounts
            .iter()
            .find(|mount| mount.directory_path == directory)
    }

    pub fn remove_mount(&mut self, id: i64) -> Option<Mount> {
        let index = self.mounts.iter().position(|mount| mount.id == id)?;
        Some(self.mounts.remove(index))
    }
}

fn generate_node_id() -> u16 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = u128::from(std::process::id());
    ((nanos ^ (pid.wrapping_mul(0x9E37_79B9))) & u128::from(MAX_NODE_ID)) as u16
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn mount(id: i64, directory: &Path) -> Mount {
        Mount {
            id,
            name: format!("mount-{id}"),
            project_path: directory.parent().unwrap().to_path_buf(),
            directory_path: directory.to_path_buf(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn creates_config_once_and_persists_node_id() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");

        let first = Config::load_or_create(&path).unwrap();
        assert!(path.is_file());
        assert!(first.node_id <= MAX_NODE_ID);

        let second = Config::load_or_create(&path).unwrap();
        assert_eq!(first.node_id, second.node_id);
        assert!(second.mounts.is_empty());
    }

    #[test]
    fn round_trips_mounts_through_toml() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let directory = temp.path().join("project/notes");
        fs::create_dir_all(&directory).unwrap();

        let mut config = Config::load_or_create(&path).unwrap();
        config.mounts.push(mount(1, &directory));
        config.save(&path).unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.node_id, config.node_id);
        assert_eq!(loaded.mounts, config.mounts);
    }

    #[test]
    fn rejects_nested_mounts() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let notes = project.join("notes");
        let nested = notes.join("decisions");
        fs::create_dir_all(&nested).unwrap();

        let config = Config {
            node_id: 1,
            mounts: vec![mount(1, &notes), mount(2, &nested)],
        };

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("nested"), "{error}");
    }

    #[test]
    fn rejects_duplicate_directories() {
        let temp = tempdir().unwrap();
        let notes = temp.path().join("project/notes");
        fs::create_dir_all(&notes).unwrap();

        let config = Config {
            node_id: 1,
            mounts: vec![mount(1, &notes), mount(2, &notes)],
        };

        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("twice"), "{error}");
    }

    #[test]
    fn rejects_invalid_node_id() {
        let config = Config {
            node_id: 1024,
            mounts: Vec::new(),
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn remove_mount_returns_the_removed_entry() {
        let temp = tempdir().unwrap();
        let notes = temp.path().join("project/notes");
        fs::create_dir_all(&notes).unwrap();

        let mut config = Config {
            node_id: 1,
            mounts: vec![mount(1, &notes)],
        };
        let removed = config.remove_mount(1).unwrap();
        assert_eq!(removed.id, 1);
        assert!(config.mounts.is_empty());
        assert!(config.remove_mount(1).is_none());
    }
}
