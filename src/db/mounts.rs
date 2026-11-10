//! Mount registry queries.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::DateTime;
use sqlx::Row;

use crate::config::Mount;

use super::{Database, DbError, Result};

impl Database {
    /// Makes the database registry match `config.toml`.
    ///
    /// Removed mounts are marked inactive instead of being deleted, so their
    /// documents, metadata, and knowledge links are preserved.
    pub async fn sync_mounts(&self, mounts: &[Mount]) -> Result<()> {
        let mut transaction = self.pool.begin().await?;

        for mount in mounts {
            let project_path = mount.project_path.to_string_lossy().into_owned();
            let directory_path = mount.directory_path.to_string_lossy().into_owned();

            sqlx::query(
                "INSERT INTO mounts
                    (id, name, project_path, directory_path, created_at, is_active)
                 VALUES (?, ?, ?, ?, ?, 1)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    project_path = excluded.project_path,
                    directory_path = excluded.directory_path,
                    created_at = excluded.created_at,
                    is_active = 1",
            )
            .bind(mount.id)
            .bind(&mount.name)
            .bind(project_path)
            .bind(directory_path)
            .bind(mount.created_at.timestamp_millis())
            .execute(&mut *transaction)
            .await?;
        }

        let keep: HashSet<i64> = mounts.iter().map(|mount| mount.id).collect();
        let registered: Vec<i64> = sqlx::query("SELECT id FROM mounts")
            .fetch_all(&mut *transaction)
            .await?
            .iter()
            .map(|row| row.get("id"))
            .collect();

        for id in registered {
            if !keep.contains(&id) {
                sqlx::query("UPDATE mounts SET is_active = 0 WHERE id = ?")
                    .bind(id)
                    .execute(&mut *transaction)
                    .await?;
            }
        }

        transaction.commit().await?;
        Ok(())
    }

    /// Finds an inactive mount for a directory so it can be restored with the
    /// same ID and its existing data.
    pub async fn inactive_mount_by_directory(&self, directory: &str) -> Result<Option<Mount>> {
        let row = sqlx::query(
            "SELECT id, name, project_path, directory_path, created_at
             FROM mounts WHERE directory_path = ? AND is_active = 0
             LIMIT 1",
        )
        .bind(directory)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let created_at: i64 = row.try_get("created_at")?;
        Ok(Some(Mount {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            project_path: PathBuf::from(row.try_get::<String, _>("project_path")?),
            directory_path: PathBuf::from(row.try_get::<String, _>("directory_path")?),
            created_at: DateTime::from_timestamp_millis(created_at)
                .ok_or(DbError::InvalidTimestamp(created_at))?,
        }))
    }
}
