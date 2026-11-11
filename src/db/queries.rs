//! Read queries for documents, links, and metadata.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::core::document::Document;

use super::{Database, DbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRow {
    pub source_id: i64,
    pub source_path: String,
    pub target_id: Option<i64>,
    pub target: String,
    pub anchor: Option<String>,
    pub kind: String,
    pub origin: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetadataEntry {
    pub key: String,
    pub value_json: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

impl Database {
    pub async fn document_counts(&self) -> Result<HashMap<i64, i64>> {
        let rows = sqlx::query(
            "SELECT d.mount_id, COUNT(*) AS document_count
             FROM documents d JOIN mounts m ON m.id = d.mount_id
             WHERE m.is_active = 1 AND d.missing_at IS NULL
             GROUP BY d.mount_id",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut counts = HashMap::new();
        for row in rows {
            counts.insert(row.try_get("mount_id")?, row.try_get("document_count")?);
        }
        Ok(counts)
    }

    /// Lists visible documents for the viewer and CLI.
    pub async fn list_documents(&self, mount_id: Option<i64>) -> Result<Vec<Document>> {
        let rows = sqlx::query(
            "SELECT d.id, d.mount_id, d.relative_path, d.title, d.content_hash,
                    d.modified_at, d.indexed_at
             FROM documents d JOIN mounts m ON m.id = d.mount_id
             WHERE d.missing_at IS NULL AND m.is_active = 1
               AND (? IS NULL OR d.mount_id = ?)
             ORDER BY d.mount_id, d.relative_path",
        )
        .bind(mount_id)
        .bind(mount_id)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(map_document).collect()
    }

    /// Lists every known document, including ones whose files are missing.
    pub async fn documents_for_mount(&self, mount_id: i64) -> Result<Vec<Document>> {
        let rows = sqlx::query(
            "SELECT id, mount_id, relative_path, title, content_hash, modified_at, indexed_at
             FROM documents WHERE mount_id = ? ORDER BY relative_path",
        )
        .bind(mount_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(map_document).collect()
    }

    pub async fn get_document(&self, id: i64) -> Result<Option<Document>> {
        let rows = sqlx::query(
            "SELECT id, mount_id, relative_path, title, content_hash, modified_at, indexed_at
             FROM documents WHERE id = ?",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        rows.first().map(map_document).transpose()
    }

    pub async fn document_by_path(
        &self,
        mount_id: i64,
        relative_path: &str,
    ) -> Result<Option<Document>> {
        let rows = sqlx::query(
            "SELECT id, mount_id, relative_path, title, content_hash, modified_at, indexed_at
             FROM documents WHERE mount_id = ? AND relative_path = ?",
        )
        .bind(mount_id)
        .bind(relative_path)
        .fetch_all(&self.pool)
        .await?;
        rows.first().map(map_document).transpose()
    }

    pub async fn missing_paths(&self, mount_id: i64) -> Result<HashSet<String>> {
        let rows = sqlx::query(
            "SELECT relative_path FROM documents
             WHERE mount_id = ? AND missing_at IS NOT NULL",
        )
        .bind(mount_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get("relative_path"))
            .collect::<std::result::Result<_, sqlx::Error>>()?)
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

    pub async fn links(&self, document_id: i64, backlinks: bool) -> Result<Vec<LinkRow>> {
        let markdown_column = if backlinks {
            "target_document_id"
        } else {
            "source_document_id"
        };
        let knowledge_column = if backlinks {
            "target_document_id"
        } else {
            "source_document_id"
        };
        let markdown_sql = format!(
            "SELECT l.source_document_id, d.relative_path, l.target_document_id,
                    COALESCE(l.target_uri, '') AS target_uri, l.target_anchor,
                    l.source_kind AS kind, 'markdown' AS origin
             FROM markdown_links l JOIN documents d ON d.id = l.source_document_id
             WHERE l.{markdown_column} = ?"
        );
        let knowledge_sql = format!(
            "SELECT k.source_document_id, d.relative_path, k.target_document_id,
                    COALESCE(k.target_uri, '') AS target_uri, NULL AS target_anchor,
                    k.relation_type AS kind, 'knowledge' AS origin
             FROM knowledge_links k JOIN documents d ON d.id = k.source_document_id
             WHERE k.{knowledge_column} = ?"
        );

        let mut rows = sqlx::query(&markdown_sql)
            .bind(document_id)
            .fetch_all(&self.pool)
            .await?;
        rows.extend(
            sqlx::query(&knowledge_sql)
                .bind(document_id)
                .fetch_all(&self.pool)
                .await?,
        );

        let mut links: Vec<LinkRow> = rows
            .iter()
            .map(|row| {
                Ok(LinkRow {
                    source_id: row.try_get("source_document_id")?,
                    source_path: row.try_get("relative_path")?,
                    target_id: row.try_get("target_document_id")?,
                    target: row.try_get("target_uri")?,
                    anchor: row.try_get("target_anchor")?,
                    kind: row.try_get("kind")?,
                    origin: row.try_get("origin")?,
                })
            })
            .collect::<Result<_>>()?;
        links.sort_by(|left, right| {
            left.source_path
                .cmp(&right.source_path)
                .then_with(|| left.origin.cmp(&right.origin))
                .then_with(|| left.kind.cmp(&right.kind))
        });
        Ok(links)
    }

    pub async fn metadata(&self, document_id: i64) -> Result<Vec<MetadataEntry>> {
        let rows = sqlx::query(
            "SELECT key, value_json, updated_at, updated_by
             FROM document_metadata WHERE document_id = ? ORDER BY key",
        )
        .bind(document_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let updated_at: i64 = row.try_get("updated_at")?;
                Ok(MetadataEntry {
                    key: row.try_get("key")?,
                    value_json: row.try_get("value_json")?,
                    updated_at: DateTime::from_timestamp_millis(updated_at)
                        .ok_or(DbError::InvalidTimestamp(updated_at))?,
                    updated_by: row.try_get("updated_by")?,
                })
            })
            .collect()
    }

    pub async fn set_metadata(
        &self,
        document_id: i64,
        key: &str,
        value_json: &str,
        updated_by: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO document_metadata (document_id, key, value_json, updated_at, updated_by)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(document_id, key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at,
                updated_by = excluded.updated_by",
        )
        .bind(document_id)
        .bind(key)
        .bind(value_json)
        .bind(Utc::now().timestamp_millis())
        .bind(updated_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_metadata(&self, document_id: i64, key: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM document_metadata WHERE document_id = ? AND key = ?")
            .bind(document_id)
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_knowledge_link(
        &self,
        source_document_id: i64,
        target_document_id: Option<i64>,
        target_uri: Option<&str>,
        relation_type: &str,
        description: Option<&str>,
        provenance: Option<&str>,
        created_by: &str,
        status: &str,
    ) -> Result<i64> {
        let now = Utc::now().timestamp_millis();
        Ok(sqlx::query_scalar(
            "INSERT INTO knowledge_links
                (source_document_id, target_document_id, target_uri, relation_type,
                 description, provenance, created_by, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
        )
        .bind(source_document_id)
        .bind(target_document_id)
        .bind(target_uri)
        .bind(relation_type)
        .bind(description)
        .bind(provenance)
        .bind(created_by)
        .bind(status)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn delete_knowledge_link(&self, id: i64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM knowledge_links WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
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
