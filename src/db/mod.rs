//! SQLite storage for the mount registry and derived document index.
//!
//! SQLx-specific types stay in this module tree; callers work with domain
//! types. Domain logic such as Markdown parsing and link resolution lives in
//! `core`/`index`, so this layer only persists what it is given.

pub mod mounts;
pub mod queries;
pub mod sync;

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

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

        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)?;
            let permissions = file.metadata()?.permissions();
            let restricted = permissions.mode() & !0o077;
            if restricted != permissions.mode() {
                file.set_permissions(std::fs::Permissions::from_mode(restricted))?;
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mount;
    use crate::core::document::Document;
    use crate::core::reference::{self, ResolvedReference};
    use chrono::Utc;
    use sqlx::Row;
    use std::collections::HashMap;
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

    fn document(
        id: i64,
        mount_id: i64,
        relative_path: &str,
        title: &str,
        content_hash: &str,
    ) -> Document {
        let now = Utc::now();
        Document {
            id,
            mount_id,
            relative_path: relative_path.into(),
            title: title.into(),
            content_hash: content_hash.into(),
            modified_at: now,
            indexed_at: now,
        }
    }

    /// Persists the given documents and all of their links, like a full sync.
    async fn apply(
        database: &Database,
        mount_id: i64,
        documents: &[Document],
        upserts: &[Document],
        deletes: &[String],
        bodies: &HashMap<i64, String>,
    ) {
        let affected: Vec<i64> = documents.iter().map(|document| document.id).collect();
        let mut references = Vec::new();
        for document in documents {
            let Some(body) = bodies.get(&document.id) else {
                continue;
            };
            for item in reference::extract(body) {
                let target_id = reference::resolve(&item, document, documents);
                references.push(ResolvedReference {
                    source_id: document.id,
                    target_id,
                    target: item.target,
                    anchor: item.anchor,
                    raw: item.raw,
                    kind: item.kind,
                });
            }
        }
        database
            .apply_mount_sync(mount_id, upserts, deletes, &references, &affected)
            .await
            .unwrap();
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
    async fn title_migration_marks_existing_documents_for_rescan() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("test.sqlite");
        let database = Database::open(&path).await.unwrap();
        database
            .sync_mounts(&[mount(1, &temp.path().join("project/notes"))])
            .await
            .unwrap();
        let document = document(10, 1, "page.md", "old", "old-hash");
        let bodies = HashMap::from([(10, "# old\n".to_string())]);
        apply(
            &database,
            1,
            std::slice::from_ref(&document),
            std::slice::from_ref(&document),
            &[],
            &bodies,
        )
        .await;
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 2")
            .execute(&database.pool)
            .await
            .unwrap();
        database.pool.close().await;
        let reopened = Database::open(&path).await.unwrap();
        assert_eq!(
            reopened.documents_for_mount(1).await.unwrap()[0].content_hash,
            ""
        );
    }

    #[tokio::test]
    async fn sync_mounts_prunes_stale_rows_and_cascades_documents() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        let first = mount(1, &temp.path().join("project/notes"));
        let second = mount(2, &temp.path().join("project/other"));

        database
            .sync_mounts(&[first.clone(), second])
            .await
            .unwrap();
        let document = document(10, 2, "page.md", "Page", "hash");
        let bodies = HashMap::from([(10, "# Page\n".to_string())]);
        apply(
            &database,
            2,
            std::slice::from_ref(&document),
            std::slice::from_ref(&document),
            &[],
            &bodies,
        )
        .await;

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
    async fn moving_document_rebuilds_reference_rows_from_content() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        database
            .sync_mounts(&[mount(1, &temp.path().join("project/notes"))])
            .await
            .unwrap();
        let bodies = HashMap::from([(10, "[Page](page.md)\n".to_string())]);
        let mut document = document(10, 1, "old.md", "Page", "hash");
        apply(
            &database,
            1,
            std::slice::from_ref(&document),
            std::slice::from_ref(&document),
            &[],
            &bodies,
        )
        .await;

        document.relative_path = "new.md".into();
        apply(
            &database,
            1,
            std::slice::from_ref(&document),
            std::slice::from_ref(&document),
            &["old.md".into()],
            &bodies,
        )
        .await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM \"references\" WHERE source_document_id = 10")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let path: String = sqlx::query_scalar("SELECT relative_path FROM documents WHERE id = 10")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(path, "new.md");
    }

    #[tokio::test]
    async fn applies_upserts_and_deletes() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        database
            .sync_mounts(&[mount(1, &temp.path().join("project/notes"))])
            .await
            .unwrap();
        let document = document(10, 1, "page.md", "Page", "hash");
        let bodies = HashMap::from([(10, "# Page\n".to_string())]);
        apply(
            &database,
            1,
            std::slice::from_ref(&document),
            std::slice::from_ref(&document),
            &[],
            &bodies,
        )
        .await;
        assert_eq!(database.documents_for_mount(1).await.unwrap().len(), 1);

        apply(
            &database,
            1,
            &[],
            &[],
            &["page.md".to_string()],
            &HashMap::new(),
        )
        .await;
        assert!(database.documents_for_mount(1).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn updating_content_keeps_inbound_references() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        database
            .sync_mounts(&[mount(1, &temp.path().join("project/notes"))])
            .await
            .unwrap();
        let source = document(10, 1, "source.md", "Source", "source");
        let mut target = document(20, 1, "target.md", "Target", "v1");
        let mut bodies = HashMap::from([
            (10, "[Target](target.md)\n".to_string()),
            (20, "# Target\n".to_string()),
        ]);
        let documents = [source.clone(), target.clone()];
        apply(
            &database,
            1,
            &documents,
            &[source, target.clone()],
            &[],
            &bodies,
        )
        .await;

        target.content_hash = "v2".into();
        bodies.insert(20, "# Target\n\nedited\n".to_string());
        let documents = [documents[0].clone(), target.clone()];
        apply(&database, 1, &documents, &[target], &[], &bodies).await;

        let target_id: Option<i64> = sqlx::query_scalar(
            "SELECT target_document_id FROM \"references\" WHERE source_document_id = 10",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(target_id, Some(20));
    }
}
