//! Mount registration and registry operations.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;

use crate::config::{AppPaths, Config, Mount};
use crate::core::id::Snowflake;
use crate::db::Database;
use crate::index::indexer::{self, SyncReport};
use crate::storage::filesystem::MountRoot;

pub struct MountResult {
    pub mount: Mount,
    pub report: SyncReport,
    pub already_registered: bool,
}

pub struct MountRow {
    pub mount: Mount,
    pub documents: i64,
}

pub async fn register(
    config: &mut Config,
    paths: &AppPaths,
    database: &Database,
    ids: &mut Snowflake,
    project: &Path,
    directory: &Path,
    name: Option<String>,
) -> Result<MountResult> {
    let project_path = canonical_directory(project)?;
    let requested = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        project_path.join(directory)
    };
    let directory_path = canonical_directory(&requested)?;

    if !directory_path.starts_with(&project_path) {
        bail!(
            "mount directory {} must be inside the project {}",
            directory_path.display(),
            project_path.display()
        );
    }

    if let Some(existing) = config.find_by_directory(&directory_path) {
        let existing = existing.clone();
        database.sync_mounts(&config.mounts).await?;
        let root = MountRoot::new(&existing.directory_path).with_context(|| {
            format!(
                "mount directory is no longer accessible: {}",
                existing.directory_path.display()
            )
        })?;
        let report = indexer::sync_mount(database, existing.id, &root, ids)
            .await
            .with_context(|| format!("cannot index mount {}", existing.id))?;
        return Ok(MountResult {
            mount: existing,
            report,
            already_registered: true,
        });
    }

    for other in &config.mounts {
        if directory_path.starts_with(&other.directory_path)
            || other.directory_path.starts_with(&directory_path)
        {
            bail!(
                "mount directory {} overlaps with existing mount \"{}\" ({})",
                directory_path.display(),
                other.name,
                other.directory_path.display()
            );
        }
    }

    let name = name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| default_name(&directory_path));

    let mount = Mount {
        id: ids.next_id().context("cannot generate a mount id")?,
        name,
        project_path,
        directory_path,
        created_at: Utc::now(),
    };

    config.mounts.push(mount.clone());
    if let Err(error) = config.validate() {
        config.mounts.pop();
        return Err(error);
    }

    config.save(&paths.config_file())?;
    database.sync_mounts(&config.mounts).await?;

    let root = MountRoot::new(&mount.directory_path)?;
    let report = indexer::sync_mount(database, mount.id, &root, ids)
        .await
        .with_context(|| {
            format!(
                "mount {} was registered but indexing failed; run `mindmount index rescan`",
                mount.id
            )
        })?;

    Ok(MountResult {
        mount,
        report,
        already_registered: false,
    })
}

pub async fn list(config: &Config, database: &Database) -> Result<Vec<MountRow>> {
    database.sync_mounts(&config.mounts).await?;
    let counts = database.document_counts().await?;
    Ok(config
        .mounts
        .iter()
        .map(|mount| MountRow {
            mount: mount.clone(),
            documents: counts.get(&mount.id).copied().unwrap_or(0),
        })
        .collect())
}

pub async fn unmount(
    config: &mut Config,
    paths: &AppPaths,
    database: &Database,
    mount_id: i64,
) -> Result<Mount> {
    let removed = config
        .remove_mount(mount_id)
        .with_context(|| format!("mount {} is not registered", mount_id))?;

    config.save(&paths.config_file())?;
    database.sync_mounts(&config.mounts).await?;
    Ok(removed)
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("cannot access directory {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("not a directory: {}", path.display());
    }
    Ok(canonical)
}

fn default_name(directory: &Path) -> String {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("mount")
        .to_string()
}
