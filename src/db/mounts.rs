//! Mount registry queries.

use std::collections::HashSet;

use sqlx::Row;

use crate::config::Mount;

use super::{Database, Result};

impl Database {
    /// Makes the database registry match `config.toml`, deleting rows for
    /// mounts that are no longer registered.
    pub async fn sync_mounts(&self, mounts: &[Mount]) -> Result<()> {
        let mut transaction = self.pool.begin().await?;

        for mount in mounts {
            let project_path = mount.project_path.to_string_lossy().into_owned();
            let directory_path = mount.directory_path.to_string_lossy().into_owned();

            sqlx::query(
                "INSERT INTO mounts (id, name, project_path, directory_path, created_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    project_path = excluded.project_path,
                    directory_path = excluded.directory_path,
                    created_at = excluded.created_at",
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
                sqlx::query("DELETE FROM mounts WHERE id = ?")
                    .bind(id)
                    .execute(&mut *transaction)
                    .await?;
            }
        }

        transaction.commit().await?;
        Ok(())
    }
}
