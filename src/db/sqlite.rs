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
use crate::index::references;

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
    #[error("missing search content for document {0}")]
    MissingContent(i64),
}

pub type Result<T> = std::result::Result<T, DbError>;

pub struct Database {
    pool: SqlitePool,
}

#[derive(Debug)]
pub struct LinkRow {
    pub source_id: i64,
    pub source_path: String,
    pub target_id: Option<i64>,
    pub target: String,
    pub anchor: Option<String>,
    pub kind: String,
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

    /// Searches literal text, using trigram lookup for queries of at least three characters.
    pub async fn search_documents(
        &self,
        query: &str,
        mount_id: Option<i64>,
    ) -> Result<Vec<Document>> {
        let sql = if query.chars().count() >= 3 {
            "SELECT d.id, d.mount_id, d.relative_path, d.title, d.content_hash, d.modified_at, d.indexed_at
             FROM document_search s JOIN documents d ON d.id = s.rowid
             WHERE document_search MATCH ? AND (? IS NULL OR d.mount_id = ?)
             ORDER BY d.mount_id, d.relative_path"
        } else {
            "SELECT d.id, d.mount_id, d.relative_path, d.title, d.content_hash, d.modified_at, d.indexed_at
             FROM document_search s JOIN documents d ON d.id = s.rowid
             WHERE (instr(lower(s.title), lower(?1)) > 0 OR instr(lower(s.body), lower(?1)) > 0)
             AND (?2 IS NULL OR d.mount_id = ?3)
             ORDER BY d.mount_id, d.relative_path"
        };
        let term = if query.chars().count() >= 3 {
            format!("\"{}\"", query.replace('"', "\"\""))
        } else {
            query.to_owned()
        };
        let rows = sqlx::query(sql)
            .bind(term)
            .bind(mount_id)
            .bind(mount_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(map_document).collect()
    }

    pub async fn unindexed_search_count(&self, mount_id: Option<i64>) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM documents d LEFT JOIN document_search s ON s.rowid = d.id
             WHERE (s.rowid IS NULL OR d.content_hash = '') AND (? IS NULL OR d.mount_id = ?)",
        )
        .bind(mount_id)
        .bind(mount_id)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn links(&self, document_id: i64, backlinks: bool) -> Result<Vec<LinkRow>> {
        let column = if backlinks {
            "r.target_document_id"
        } else {
            "r.source_document_id"
        };
        let sql = format!("SELECT r.source_document_id, d.relative_path, r.target_document_id, r.target_uri, r.target_anchor, r.source_kind FROM \"references\" r JOIN documents d ON d.id = r.source_document_id WHERE {column} = ? ORDER BY d.relative_path, r.id");
        let rows = sqlx::query(&sql)
            .bind(document_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(LinkRow {
                    source_id: row.try_get("source_document_id")?,
                    source_path: row.try_get("relative_path")?,
                    target_id: row.try_get("target_document_id")?,
                    target: row.try_get("target_uri")?,
                    anchor: row.try_get("target_anchor")?,
                    kind: row.try_get("source_kind")?,
                })
            })
            .collect()
    }

    pub async fn document_exists(&self, id: i64) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents WHERE id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?
                > 0,
        )
    }

    /// Applies one mount's index changes in a single transaction.
    #[cfg(test)]
    pub(crate) async fn apply_mount_sync(
        &self,
        mount_id: i64,
        upserts: &[Document],
        deletes: &[String],
    ) -> Result<()> {
        self.apply_mount_sync_inner(mount_id, upserts, deletes, &HashMap::new())
            .await
    }

    pub(crate) async fn apply_mount_sync_with_content(
        &self,
        mount_id: i64,
        upserts: &[Document],
        deletes: &[String],
        content: &HashMap<i64, String>,
    ) -> Result<()> {
        for document in upserts {
            if !content.contains_key(&document.id) {
                return Err(DbError::MissingContent(document.id));
            }
        }
        self.apply_mount_sync_inner(mount_id, upserts, deletes, content)
            .await
    }

    async fn apply_mount_sync_inner(
        &self,
        mount_id: i64,
        upserts: &[Document],
        deletes: &[String],
        content: &HashMap<i64, String>,
    ) -> Result<()> {
        if upserts.is_empty() && deletes.is_empty() {
            return Ok(());
        }

        let mut transaction = self.pool.begin().await?;

        let rows = sqlx::query("SELECT id, relative_path FROM documents WHERE mount_id = ?")
            .bind(mount_id)
            .fetch_all(&mut *transaction)
            .await?;
        let existing: HashMap<i64, String> = rows
            .iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("relative_path")?)))
            .collect::<std::result::Result<_, sqlx::Error>>()?;
        let deleted_paths: HashSet<&str> = deletes.iter().map(String::as_str).collect();
        let upsert_paths: HashSet<&str> = upserts
            .iter()
            .map(|doc| doc.relative_path.as_str())
            .collect();
        let moves: Vec<&Document> = upserts
            .iter()
            .filter(|doc| {
                existing.get(&doc.id).is_some_and(|old| {
                    old != &doc.relative_path
                        && (deleted_paths.contains(old.as_str())
                            || upsert_paths.contains(old.as_str()))
                })
            })
            .collect();
        let moving_ids: HashSet<i64> = moves.iter().map(|doc| doc.id).collect();
        let move_destinations: HashSet<&str> =
            moves.iter().map(|doc| doc.relative_path.as_str()).collect();

        // Move through paths that cannot occur on disk, allowing swaps and
        // replacements while preserving the rows (and their references).
        for document in &moves {
            sqlx::query("UPDATE documents SET relative_path = ? WHERE id = ? AND mount_id = ?")
                .bind(format!("\0mindmount-move-{}", document.id))
                .bind(document.id)
                .bind(mount_id)
                .execute(&mut *transaction)
                .await?;
        }
        for (id, path) in &existing {
            if !moving_ids.contains(id)
                && (deleted_paths.contains(path.as_str())
                    || move_destinations.contains(path.as_str()))
            {
                sqlx::query("DELETE FROM documents WHERE id = ? AND mount_id = ?")
                    .bind(id)
                    .bind(mount_id)
                    .execute(&mut *transaction)
                    .await?;
            }
        }
        for document in moves {
            sqlx::query("UPDATE documents SET relative_path = ? WHERE id = ? AND mount_id = ?")
                .bind(&document.relative_path)
                .bind(document.id)
                .bind(mount_id)
                .execute(&mut *transaction)
                .await?;
        }

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
            if let Some(body) = content.get(&document.id) {
                sqlx::query("DELETE FROM document_search WHERE rowid = ?")
                    .bind(document.id)
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query("INSERT INTO document_search (rowid, title, body) VALUES (?, ?, ?)")
                    .bind(document.id)
                    .bind(&document.title)
                    .bind(body)
                    .execute(&mut *transaction)
                    .await?;
            }
        }

        let rows = sqlx::query("SELECT d.id, d.mount_id, d.relative_path, d.title, d.content_hash, d.modified_at, d.indexed_at, s.body FROM documents d LEFT JOIN document_search s ON d.id = s.rowid WHERE d.mount_id = ?")
            .bind(mount_id).fetch_all(&mut *transaction).await?;
        let documents: Vec<Document> = rows.iter().map(map_document).collect::<Result<_>>()?;
        for (row, document) in rows.iter().zip(&documents) {
            let body: Option<String> = row.try_get("body")?;
            let Some(body) = body else { continue };
            sqlx::query("DELETE FROM \"references\" WHERE source_document_id = ?")
                .bind(document.id)
                .execute(&mut *transaction)
                .await?;
            for reference in references::extract(&body) {
                let target_id = references::resolve(&reference, document, &documents);
                sqlx::query("INSERT INTO \"references\" (source_document_id, target_document_id, target_uri, target_anchor, raw_reference, relation_type, source_kind) VALUES (?, ?, ?, ?, ?, 'link', ?)")
                    .bind(document.id).bind(target_id).bind(&reference.target).bind(&reference.anchor)
                    .bind(&reference.raw).bind(reference.kind).execute(&mut *transaction).await?;
            }
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
    async fn refuses_upserts_without_search_content() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        let mount = mount(1, &temp.path().join("project/notes"));
        database.sync_mounts(&[mount]).await.unwrap();
        let now = Utc::now();
        let document = Document {
            id: 10,
            mount_id: 1,
            relative_path: "page.md".into(),
            title: "Page".into(),
            content_hash: "hash".into(),
            modified_at: now,
            indexed_at: now,
        };
        let error = database
            .apply_mount_sync_with_content(1, &[document], &[], &HashMap::new())
            .await
            .unwrap_err();
        assert!(matches!(error, DbError::MissingContent(10)));
        assert!(database.documents_for_mount(1).await.unwrap().is_empty());
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
    async fn search_migration_requires_rescan_of_existing_documents() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("test.sqlite");
        let database = Database::open(&path).await.unwrap();
        let mount = mount(1, &temp.path().join("project/notes"));
        database.sync_mounts(&[mount]).await.unwrap();
        let now = Utc::now();
        database
            .apply_mount_sync(
                1,
                &[Document {
                    id: 10,
                    mount_id: 1,
                    relative_path: "page.md".into(),
                    title: "Page".into(),
                    content_hash: "hash".into(),
                    modified_at: now,
                    indexed_at: now,
                }],
                &[],
            )
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER document_search_delete")
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DROP TRIGGER document_search_update_id")
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DROP TABLE document_search")
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 3")
            .execute(&database.pool)
            .await
            .unwrap();
        database.pool.close().await;
        let reopened = Database::open(&path).await.unwrap();
        assert_eq!(reopened.unindexed_search_count(None).await.unwrap(), 1);
        assert_eq!(
            reopened.documents_for_mount(1).await.unwrap()[0].content_hash,
            ""
        );
    }

    #[tokio::test]
    async fn title_migration_marks_existing_documents_for_rescan() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("test.sqlite");
        let database = Database::open(&path).await.unwrap();
        let mount = mount(1, &temp.path().join("project/notes"));
        database.sync_mounts(&[mount]).await.unwrap();
        let now = Utc::now();
        database
            .apply_mount_sync(
                1,
                &[Document {
                    id: 10,
                    mount_id: 1,
                    relative_path: "page.md".into(),
                    title: "old".into(),
                    content_hash: "old-hash".into(),
                    modified_at: now,
                    indexed_at: now,
                }],
                &[],
            )
            .await
            .unwrap();
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
    async fn moving_document_preserves_reference_rows() {
        let temp = tempdir().unwrap();
        let database = Database::open(&temp.path().join("test.sqlite"))
            .await
            .unwrap();
        let mount = mount(1, &temp.path().join("project/notes"));
        database.sync_mounts(&[mount]).await.unwrap();
        let now = Utc::now();
        let mut document = Document {
            id: 10,
            mount_id: 1,
            relative_path: "old.md".into(),
            title: "Page".into(),
            content_hash: "hash".into(),
            modified_at: now,
            indexed_at: now,
        };
        database
            .apply_mount_sync(1, &[document.clone()], &[])
            .await
            .unwrap();
        sqlx::query("INSERT INTO \"references\" (id, source_document_id, target_document_id, raw_reference, relation_type, source_kind) VALUES (1, 10, 10, 'page', 'link', 'markdown')")
            .execute(&database.pool).await.unwrap();
        document.relative_path = "new.md".into();
        database
            .apply_mount_sync(1, &[document], &["old.md".into()])
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM \"references\" WHERE source_document_id = 10 AND target_document_id = 10")
            .fetch_one(&database.pool).await.unwrap();
        assert_eq!(count, 1);
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
