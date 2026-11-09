//! Index and query operations shared by the CLI, MCP, and web layers.

use anyhow::{bail, Result};

use crate::config::Config;
use crate::core::document::Document;
use crate::core::id::Snowflake;
use crate::db::queries::LinkRow;
use crate::db::Database;
use crate::index::indexer::{self, SyncOutcome};

pub async fn rescan(
    config: &Config,
    database: &Database,
    ids: &mut Snowflake,
) -> Result<SyncOutcome> {
    indexer::sync_all(config, database, ids).await
}

pub async fn pages(
    config: &Config,
    database: &Database,
    mount_id: Option<i64>,
) -> Result<Vec<Document>> {
    database.sync_mounts(&config.mounts).await?;

    if let Some(mount_id) = mount_id {
        if config.find_by_id(mount_id).is_none() {
            bail!("mount {} is not registered", mount_id);
        }
    }

    database.list_documents(mount_id).await.map_err(Into::into)
}

pub async fn search(
    config: &Config,
    database: &Database,
    query: &str,
    mount_id: Option<i64>,
) -> Result<Vec<Document>> {
    if query.is_empty() {
        bail!("search query must not be empty");
    }
    if let Some(id) = mount_id {
        if config.find_by_id(id).is_none() {
            bail!("mount {} is not registered", id);
        }
    }
    database.sync_mounts(&config.mounts).await?;
    let missing = database.unindexed_search_count(mount_id).await?;
    if missing > 0 {
        bail!("{missing} pages are not searchable yet; run `mindmount index rescan`");
    }
    database
        .search_documents(query, mount_id)
        .await
        .map_err(Into::into)
}

pub async fn links(
    config: &Config,
    database: &Database,
    document_id: i64,
    backlinks: bool,
) -> Result<Vec<LinkRow>> {
    database.sync_mounts(&config.mounts).await?;
    if !database.document_exists(document_id).await? {
        bail!("page {} is not indexed", document_id);
    }
    let missing = database.unindexed_search_count(None).await?;
    if missing > 0 {
        bail!("{missing} pages need indexing; run `mindmount index rescan`");
    }
    database
        .links(document_id, backlinks)
        .await
        .map_err(Into::into)
}
