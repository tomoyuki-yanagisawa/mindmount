//! Index and query operations shared by the CLI, MCP, and web layers.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::core::document::Document;
use crate::core::id::Snowflake;
use crate::db::queries::LinkRow;
use crate::db::Database;
use crate::index::indexer::{self, SyncOutcome, SyncReport};
use crate::search::{self, SearchHit, SearchOptions};
use crate::storage::filesystem::MountRoot;

const SEARCH_LIMIT: usize = 100;

pub async fn rescan(
    config: &Config,
    database: &Database,
    ids: &mut Snowflake,
) -> Result<SyncOutcome> {
    indexer::sync_all(config, database, ids).await
}

pub async fn resync_mount(
    config: &Config,
    database: &Database,
    ids: &mut Snowflake,
    mount_id: i64,
) -> Result<SyncReport> {
    let mount = config
        .find_by_id(mount_id)
        .with_context(|| format!("mount {mount_id} is not registered"))?;
    let root = MountRoot::new(&mount.directory_path)?;
    database.sync_mounts(&config.mounts).await?;
    indexer::sync_mount(database, mount_id, &root, ids).await
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
) -> Result<Vec<SearchHit>> {
    if query.trim().is_empty() {
        bail!("search query must not be empty");
    }
    if let Some(id) = mount_id {
        if config.find_by_id(id).is_none() {
            bail!("mount {} is not registered", id);
        }
    }

    let mut hits = search::search(
        config,
        SearchOptions {
            query,
            mount_id,
            limit: SEARCH_LIMIT,
        },
    )?;
    let documents = database.list_documents(mount_id).await?;
    let ids: HashMap<(i64, &str), i64> = documents
        .iter()
        .map(|document| {
            (
                (document.mount_id, document.relative_path.as_str()),
                document.id,
            )
        })
        .collect();
    for hit in &mut hits {
        hit.document_id = ids
            .get(&(hit.mount_id, hit.relative_path.as_str()))
            .copied();
    }
    Ok(hits)
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
    database
        .links(document_id, backlinks)
        .await
        .map_err(Into::into)
}
