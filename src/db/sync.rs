//! Applies one mount's derived index changes in a single transaction.

use std::collections::HashMap;

use chrono::Utc;

use crate::core::document::Document;
use crate::core::reference::ResolvedReference;

use super::{Database, Result};

impl Database {
    /// Applies changed and missing files for one mount.
    ///
    /// Document rows are never deleted. Missing files get `missing_at`, and
    /// re-appearing files clear it. Because updates happen in place, metadata
    /// and knowledge links survive syncs and renames.
    pub(crate) async fn apply_mount_sync(
        &self,
        mount_id: i64,
        upserts: &[Document],
        missing: &[String],
        references: &[ResolvedReference],
        affected_sources: &[i64],
    ) -> Result<()> {
        if upserts.is_empty() && missing.is_empty() {
            return Ok(());
        }

        let mut transaction = self.pool.begin().await?;

        let existing: HashMap<i64, String> =
            sqlx::query("SELECT id, relative_path FROM documents WHERE mount_id = ?")
                .bind(mount_id)
                .fetch_all(&mut *transaction)
                .await?
                .iter()
                .map(|row| {
                    use sqlx::Row;
                    Ok((row.try_get("id")?, row.try_get("relative_path")?))
                })
                .collect::<std::result::Result<_, sqlx::Error>>()?;

        // Move existing rows aside before rewriting paths, so renames and
        // swaps do not collide while keeping the same document IDs.
        for document in upserts {
            if existing.contains_key(&document.id) {
                sqlx::query("UPDATE documents SET relative_path = ? WHERE mount_id = ? AND id = ?")
                    .bind(format!("\0move-{}", document.id))
                    .bind(mount_id)
                    .bind(document.id)
                    .execute(&mut *transaction)
                    .await?;
            }
        }

        let now = Utc::now().timestamp_millis();
        for path in missing {
            sqlx::query(
                "UPDATE documents SET missing_at = ? WHERE mount_id = ? AND relative_path = ?",
            )
            .bind(now)
            .bind(mount_id)
            .bind(path)
            .execute(&mut *transaction)
            .await?;
        }

        // A non-upsert row occupying a destination path becomes an orphan
        // instead of being deleted.
        for document in upserts {
            sqlx::query(
                "UPDATE documents
                 SET relative_path = '.mindmount-orphan-' || id, missing_at = ?
                 WHERE mount_id = ? AND relative_path = ? AND id != ?",
            )
            .bind(now)
            .bind(mount_id)
            .bind(&document.relative_path)
            .bind(document.id)
            .execute(&mut *transaction)
            .await?;
        }

        for document in upserts {
            debug_assert_eq!(document.mount_id, mount_id);
            if existing.contains_key(&document.id) {
                sqlx::query(
                    "UPDATE documents SET
                        relative_path = ?, title = ?, content_hash = ?,
                        modified_at = ?, indexed_at = ?, missing_at = NULL
                     WHERE mount_id = ? AND id = ?",
                )
                .bind(&document.relative_path)
                .bind(&document.title)
                .bind(&document.content_hash)
                .bind(document.modified_at.timestamp_millis())
                .bind(document.indexed_at.timestamp_millis())
                .bind(mount_id)
                .bind(document.id)
                .execute(&mut *transaction)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO documents
                        (id, mount_id, relative_path, title, content_hash, modified_at, indexed_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
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
        }

        for source_id in affected_sources {
            sqlx::query("DELETE FROM markdown_links WHERE source_document_id = ?")
                .bind(source_id)
                .execute(&mut *transaction)
                .await?;
        }
        for reference in references {
            sqlx::query(
                "INSERT INTO markdown_links
                    (source_document_id, target_document_id, target_uri, target_anchor,
                     raw_reference, source_heading, source_line, resolution_status, source_kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(reference.source_id)
            .bind(reference.target_id)
            .bind(&reference.target)
            .bind(&reference.anchor)
            .bind(&reference.raw)
            .bind(&reference.heading)
            .bind(reference.line as i64)
            .bind(if reference.target_id.is_some() {
                "resolved"
            } else {
                "unresolved"
            })
            .bind(reference.kind)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }
}
