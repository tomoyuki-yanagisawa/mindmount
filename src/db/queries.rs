//! Read queries for documents, search, and links.

use std::collections::HashMap;

use chrono::DateTime;
use sqlx::Row;

use crate::core::document::Document;

use super::{Database, DbError, Result};

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

    pub async fn document_exists(&self, id: i64) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM documents WHERE id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?
                > 0,
        )
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
