//! Applies one mount's derived index changes in a single transaction.

use crate::core::document::Document;
use crate::core::reference::ResolvedReference;

use super::{Database, Result};

impl Database {
    /// Replaces one mount's changed documents and its references in a single
    /// transaction. Each changed file is replaced as a unit; unchanged rows are
    /// left alone.
    ///
    /// `references` holds the new links for `affected_sources`, the pages whose
    /// links must be rewritten.
    pub(crate) async fn apply_mount_sync(
        &self,
        mount_id: i64,
        upserts: &[Document],
        deletes: &[String],
        references: &[ResolvedReference],
        affected_sources: &[i64],
    ) -> Result<()> {
        if upserts.is_empty() && deletes.is_empty() {
            return Ok(());
        }

        let mut transaction = self.pool.begin().await?;

        for path in deletes {
            sqlx::query("DELETE FROM documents WHERE mount_id = ? AND relative_path = ?")
                .bind(mount_id)
                .bind(path)
                .execute(&mut *transaction)
                .await?;
        }
        // Replace each changed file as a unit. Removing both the old ID and
        // the destination path also covers renames, swaps, and pages moved
        // over an existing path.
        for document in upserts {
            sqlx::query(
                "DELETE FROM documents WHERE mount_id = ? AND (id = ? OR relative_path = ?)",
            )
            .bind(mount_id)
            .bind(document.id)
            .bind(&document.relative_path)
            .execute(&mut *transaction)
            .await?;
        }

        for document in upserts {
            debug_assert_eq!(document.mount_id, mount_id);
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

        // Only pages whose own links changed, whose target was replaced, or
        // whose resolution changed are rewritten. Unreadable pages and
        // unrelated pages keep their previous links.
        for source_id in affected_sources {
            sqlx::query("DELETE FROM \"references\" WHERE source_document_id = ?")
                .bind(source_id)
                .execute(&mut *transaction)
                .await?;
        }
        for reference in references {
            sqlx::query(
                "INSERT INTO \"references\" (source_document_id, target_document_id, target_uri, target_anchor, raw_reference, relation_type, source_kind) VALUES (?, ?, ?, ?, ?, 'link', ?)",
            )
            .bind(reference.source_id)
            .bind(reference.target_id)
            .bind(&reference.target)
            .bind(&reference.anchor)
            .bind(&reference.raw)
            .bind(reference.kind)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }
}
