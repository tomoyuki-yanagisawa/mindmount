//! DB-owned metadata and knowledge links.

use anyhow::{bail, Result};
use serde_json::Value;

use crate::db::queries::MetadataEntry;
use crate::db::Database;

pub async fn metadata(database: &Database, document_id: i64) -> Result<Vec<MetadataEntry>> {
    require_document(database, document_id).await?;
    database.metadata(document_id).await.map_err(Into::into)
}

pub async fn set_metadata(
    database: &Database,
    document_id: i64,
    key: &str,
    value: &Value,
    updated_by: &str,
) -> Result<()> {
    if key.trim().is_empty() {
        bail!("metadata key must not be empty");
    }
    require_document(database, document_id).await?;
    database
        .set_metadata(document_id, key, &value.to_string(), updated_by)
        .await
        .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
pub async fn add_link(
    database: &Database,
    source_document_id: i64,
    target_document_id: Option<i64>,
    target_uri: Option<&str>,
    relation_type: &str,
    description: Option<&str>,
    provenance: Option<&str>,
    created_by: &str,
    status: &str,
) -> Result<i64> {
    require_document(database, source_document_id).await?;
    if relation_type.trim().is_empty() {
        bail!("relation_type must not be empty");
    }
    if !matches!(status, "proposed" | "confirmed") {
        bail!("status must be proposed or confirmed");
    }
    if let Some(target) = target_document_id {
        require_document(database, target).await?;
    }
    if target_document_id.is_none() && target_uri.is_none_or(|uri| uri.trim().is_empty()) {
        bail!("provide target_document_id or target_uri");
    }

    database
        .add_knowledge_link(
            source_document_id,
            target_document_id,
            target_uri,
            relation_type,
            description,
            provenance,
            created_by,
            status,
        )
        .await
        .map_err(Into::into)
}

pub async fn delete_link(database: &Database, id: i64) -> Result<bool> {
    database.delete_knowledge_link(id).await.map_err(Into::into)
}

async fn require_document(database: &Database, document_id: i64) -> Result<()> {
    if !database.document_exists(document_id).await? {
        bail!("page {document_id} is not indexed");
    }
    Ok(())
}
