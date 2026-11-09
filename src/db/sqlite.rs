//! SQLite storage for the mount registry and derived document index.
//!
//! SQLx-specific types stay in this module; callers work with domain types.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use chrono::DateTime;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::Row;

use crate::config::Mount;
use crate::core::document::Document;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid timestamp in database: {0}")]
    InvalidTimestamp(i64),
}

pub type Result<T> = std::result::Result<T, DbError>;

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(Self { pool })
    }

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

    pub async fn document_counts(&self) -> Result<HashMap<i64, i64>> {
        let rows = sqlx::query(
            "SELECT mount_id, COUNT(*) AS document_count
             FROM documents
             GROUP BY mount_id",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut counts = HashMap::new();
        for row in rows {
            let mount_id: i64 = row.try_get("mount_id")?;
            let document_count: i64 = row.try_get("document_count")?;
            counts.insert(mount_id, document_count);
        }
        Ok(counts)
    }

    pub async fn list_documents(&self, mount_id: Option<i64>) -> Result<Vec<Document>> {
        let rows = if let Some(mount_id) = mount_id {
            sqlx::query(
                "SELECT id, mount_id, relative_path, title, content_hash, modified_at, indexed_at
                 FROM documents
                 WHERE mount_id = ?
                 ORDER BY relative_path",
            )
            .bind(mount_id)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, mount_id, relative_path, title, content_hash, modified_at, indexed_at
                 FROM documents
                 ORDER BY mount_id, relative_path",
            )
            .fetch_all(&self.pool)
            .await?
        };

        rows.iter().map(map_document).collect()
    }

    pub async fn documents_for_mount(&self, mount_id: i64) -> Result<Vec<Document>> {
        self.list_documents(Some(mount_id)).await
    }

    /// Applies one mount's index changes in a single transaction.
    pub async fn apply_mount_sync(
        &self,
        mount_id: i64,
        upserts: &[Document],
        deletes: &[String],
    ) -> Result<()> {
        if upserts.is_empty() && deletes.is_empty() {
            return Ok(());
        }

        let mut transaction = self.pool.begin().await?;

        for document in upserts {
            debug_assert_eq!(document.mount_id, mount_id);

            sqlx::query(
                "INSERT INTO documents
                    (id, mount_id, relative_path, title, content_hash, modified_at, indexed_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(mount_id, relative_path) DO UPDATE SET
                    id = excluded.id,
                    title = excluded.title,
                    content_hash = excluded.content_hash,
                    modified_at = excluded.modified_at,
                    indexed_at = excluded.indexed_at",
            )
            .bind(document.id)
            .bind(document.mount_id)
            .bind(&document.relative_path)
            .bind(&document.title)
            .bind(&document.content_hash)
            .bind(document.modified_at.timestamp_millis())
            .bind(document.indexed_at.timestamp_millis())
            .execute(&mut *transaction)
            .await?;
        }

        for relative_path in deletes {
            sqlx::query("DELETE FROM documents WHERE mount_id = ? AND relative_path = ?")
                .bind(mount_id)
                .bind(relative_path)
                .execute(&mut *transaction)
                .await?;
        }

        transaction.commit().await?;
        Ok(())
    }
}

fn map_document(row: &sqlx::sqlite::SqliteRow) -> Result<Document> {
    let modified_at: i64 = row.try_get("modified_at")?;
    let indexed_at: i64 = row.try_get("indexed_at")?;

    Ok(Document {
        id: row.try_get("id")?,
        mount_id: row.try_get("mount_id")?,
        relative_path: row.try_get("relative_path")?,
        title: row.try_get("title")?,
        content_hash: row.try_get("content_hash")?,
        modified_at: DateTime::from_timestamp_millis(modified_at)
            .ok_or(DbError::InvalidTimestamp(modified_at))?,
        indexed_at: DateTime::from_timestamp_millis(indexed_at)
            .ok_or(DbError::InvalidTimestamp(indexed_at))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
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

    #[tokio::test]
    async fn migrations_create_registry_and_reference_tables() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();

        let names: Vec<String> = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&database.pool)
            .await
            .unwrap()
            .iter()
            .map(|row| row.get("name"))
            .collect();

        assert!(names.contains(&"mounts".to_string()));
        assert!(names.contains(&"documents".to_string()));
        assert!(names.contains(&"references".to_string()));
    }

    #[tokio::test]
    async fn sync_mounts_prunes_stale_rows_and_cascades_documents() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        let first = mount(1, &temp.path().join("project/notes"));
        let second = mount(2, &temp.path().join("project/other"));
        let now = Utc::now();

        database
            .sync_mounts(&[first.clone(), second])
            .await
            .unwrap();
        database
            .apply_mount_sync(
                2,
                &[Document {
                    id: 10,
                    mount_id: 2,
                    relative_path: "page.md".to_string(),
                    title: "Page".to_string(),
                    content_hash: "hash".to_string(),
                    modified_at: now,
                    indexed_at: now,
                }],
                &[],
            )
            .await
            .unwrap();

        database.sync_mounts(&[first]).await.unwrap();

        let mounts: i64 = sqlx::query("SELECT COUNT(*) AS count FROM mounts")
            .fetch_one(&database.pool)
            .await
            .unwrap()
            .get("count");
        let documents: i64 = sqlx::query("SELECT COUNT(*) AS count FROM documents")
            .fetch_one(&database.pool)
            .await
            .unwrap()
            .get("count");

        assert_eq!(mounts, 1);
        assert_eq!(documents, 0);
    }

    #[tokio::test]
    async fn applies_upserts_and_deletes() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        let mount = mount(1, &temp.path().join("project/notes"));
        database
            .sync_mounts(std::slice::from_ref(&mount))
            .await
            .unwrap();
        let now = Utc::now();

        let document = Document {
            id: 10,
            mount_id: 1,
            relative_path: "page.md".to_string(),
            title: "Page".to_string(),
            content_hash: "hash".to_string(),
            modified_at: now,
            indexed_at: now,
        };
        database
            .apply_mount_sync(1, std::slice::from_ref(&document), &[])
            .await
            .unwrap();
        assert_eq!(database.documents_for_mount(1).await.unwrap().len(), 1);

        database
            .apply_mount_sync(1, &[], &["page.md".to_string()])
            .await
            .unwrap();
        assert!(database.documents_for_mount(1).await.unwrap().is_empty());
    }
}
