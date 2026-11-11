//! Local HTTP API used by the CLI.
//!
//! The daemon owns the database and configuration. CLI commands talk to these
//! endpoints instead of opening SQLite themselves.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::config::{AppPaths, Config};
use crate::core::id::Snowflake;
use crate::db::Database;
use crate::usecase::{
    document::{self, DocumentTarget},
    index as index_ops, knowledge, mount,
};

#[derive(Clone)]
pub struct ApiState {
    pub paths: AppPaths,
    pub database: Arc<Database>,
    pub port: u16,
    /// Serializes configuration and index writes with the file watcher.
    pub write_lock: Arc<Mutex<()>>,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/admin/status", get(status))
        .route("/api/admin/mounts", get(list_mounts).post(add_mount))
        .route("/api/admin/mounts/sync", post(sync_all))
        .route("/api/admin/mounts/{id}", delete(remove_mount))
        .route("/api/admin/mounts/{id}/sync", post(sync_mount))
        .route("/api/admin/backup", post(backup))
        .route("/api/search", get(search))
        .route("/api/documents", get(list_documents))
        .route("/api/documents/{id}", get(read_document))
        .route("/api/documents/{id}/links", get(links))
        .route("/api/documents/{id}/backlinks", get(backlinks))
        .route("/api/documents/{id}/metadata", get(metadata))
        .route(
            "/api/documents/{id}/metadata/{key}",
            get(metadata_value)
                .put(set_metadata)
                .delete(delete_metadata),
        )
        .route("/api/knowledge-links", post(add_link))
        .route("/api/knowledge-links/{id}", delete(delete_link))
        .with_state(state)
}

async fn status(State(state): State<ApiState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "port": state.port,
    }))
}

#[derive(Debug, Deserialize)]
struct ListMountsQuery {
    all: Option<bool>,
}

async fn list_mounts(
    State(state): State<ApiState>,
    Query(query): Query<ListMountsQuery>,
) -> Result<Json<Vec<mount::MountRow>>, ApiError> {
    let config = load_config(&state)?;
    let rows = mount::list(&config, &state.database, query.all.unwrap_or(false)).await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
struct AddMountRequest {
    project: PathBuf,
    directory: PathBuf,
    name: Option<String>,
}

async fn add_mount(
    State(state): State<ApiState>,
    Json(request): Json<AddMountRequest>,
) -> Result<(StatusCode, Json<mount::MountResult>), ApiError> {
    let _guard = state.write_lock.lock().await;
    let mut config = load_config(&state)?;
    let mut ids = ids(&state, &config)?;
    let result = mount::register(
        &mut config,
        &state.paths,
        &state.database,
        &mut ids,
        &request.project,
        &request.directory,
        request.name,
    )
    .await?;
    let status = if result.already_registered {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(result)))
}

async fn remove_mount(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::config::Mount>, ApiError> {
    let _guard = state.write_lock.lock().await;
    let mut config = load_config(&state)?;
    let removed = mount::unmount(&mut config, &state.paths, &state.database, id).await?;
    Ok(Json(removed))
}

async fn sync_mount(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<crate::index::indexer::SyncReport>, ApiError> {
    let _guard = state.write_lock.lock().await;
    let config = load_config(&state)?;
    let mut ids = ids(&state, &config)?;
    let report = index_ops::resync_mount(&config, &state.database, &mut ids, id).await?;
    Ok(Json(report))
}

async fn sync_all(
    State(state): State<ApiState>,
) -> Result<Json<crate::index::indexer::SyncOutcome>, ApiError> {
    let _guard = state.write_lock.lock().await;
    let config = load_config(&state)?;
    let mut ids = ids(&state, &config)?;
    let outcome = index_ops::rescan(&config, &state.database, &mut ids).await?;
    Ok(Json(outcome))
}

#[derive(Debug, Deserialize)]
struct BackupRequest {
    path: PathBuf,
}

async fn backup(
    State(state): State<ApiState>,
    Json(request): Json<BackupRequest>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.write_lock.lock().await;
    state
        .database
        .backup(&request.path)
        .await
        .map_err(anyhow::Error::new)?;
    Ok(Json(json!({ "path": request.path })))
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    mount: Option<i64>,
    file: Option<String>,
    #[serde(default)]
    regex: bool,
}

async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<crate::search::SearchHit>>, ApiError> {
    let config = load_config(&state)?;
    let hits = index_ops::search(
        &config,
        &state.database,
        &query.q,
        query.mount,
        query.file.as_deref(),
        query.regex,
    )
    .await?;
    Ok(Json(hits))
}

#[derive(Debug, Deserialize)]
struct DocumentsQuery {
    mount: Option<i64>,
}

async fn list_documents(
    State(state): State<ApiState>,
    Query(query): Query<DocumentsQuery>,
) -> Result<Json<Vec<crate::core::document::Document>>, ApiError> {
    let config = load_config(&state)?;
    let documents = index_ops::pages(&config, &state.database, query.mount).await?;
    Ok(Json(documents))
}

async fn read_document(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<document::ReadResult>, ApiError> {
    let config = load_config(&state)?;
    let read = document::read(&config, &state.database, DocumentTarget::Id(id))
        .await
        .map_err(|error| ApiError::not_found(format!("page {id}: {error:#}")))?;
    Ok(Json(read))
}

async fn links(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<crate::db::queries::LinkRow>>, ApiError> {
    let config = load_config(&state)?;
    let rows = index_ops::links(&config, &state.database, id, false).await?;
    Ok(Json(rows))
}

async fn backlinks(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<crate::db::queries::LinkRow>>, ApiError> {
    let config = load_config(&state)?;
    let rows = index_ops::links(&config, &state.database, id, true).await?;
    Ok(Json(rows))
}

async fn metadata(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<crate::db::queries::MetadataEntry>>, ApiError> {
    let entries = knowledge::metadata(&state.database, id).await?;
    Ok(Json(entries))
}

async fn metadata_value(
    State(state): State<ApiState>,
    Path((id, key)): Path<(i64, String)>,
) -> Result<Json<Value>, ApiError> {
    let entries = knowledge::metadata(&state.database, id).await?;
    let entry = entries
        .into_iter()
        .find(|entry| entry.key == key)
        .ok_or_else(|| ApiError::not_found(format!("metadata \"{key}\" is not set")))?;
    Ok(Json(json!({
        "key": entry.key,
        "value": serde_json::from_str::<Value>(&entry.value_json).unwrap_or(Value::String(entry.value_json)),
        "updated_at": entry.updated_at,
        "updated_by": entry.updated_by,
    })))
}

#[derive(Debug, Deserialize)]
struct MetadataUpdate {
    value: Value,
}

async fn set_metadata(
    State(state): State<ApiState>,
    Path((id, key)): Path<(i64, String)>,
    Json(request): Json<MetadataUpdate>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.write_lock.lock().await;
    knowledge::set_metadata(&state.database, id, &key, &request.value, "cli").await?;
    Ok(Json(json!({ "key": key, "value": request.value })))
}

async fn delete_metadata(
    State(state): State<ApiState>,
    Path((id, key)): Path<(i64, String)>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.write_lock.lock().await;
    let deleted = knowledge::delete_metadata(&state.database, id, &key).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

#[derive(Debug, Deserialize)]
struct KnowledgeLinkRequest {
    source_document_id: i64,
    #[serde(default)]
    target_document_id: Option<i64>,
    #[serde(default)]
    target_uri: Option<String>,
    #[serde(default = "default_relation")]
    relation_type: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    provenance: Option<String>,
    #[serde(default = "default_status")]
    status: String,
}

fn default_relation() -> String {
    "related_to".into()
}

fn default_status() -> String {
    "proposed".into()
}

async fn add_link(
    State(state): State<ApiState>,
    Json(request): Json<KnowledgeLinkRequest>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.write_lock.lock().await;
    let id = knowledge::add_link(
        &state.database,
        request.source_document_id,
        request.target_document_id,
        request.target_uri.as_deref(),
        &request.relation_type,
        request.description.as_deref(),
        request.provenance.as_deref(),
        "cli",
        &request.status,
    )
    .await?;
    Ok(Json(json!({ "id": id })))
}

async fn delete_link(
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let _guard = state.write_lock.lock().await;
    let deleted = knowledge::delete_link(&state.database, id).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

fn load_config(state: &ApiState) -> Result<Config> {
    Config::load_or_create(&state.paths.config_file())
}

fn ids(state: &ApiState, config: &Config) -> Result<Snowflake> {
    Ok(Snowflake::persistent(
        config.node_id,
        &state.paths.id_state_file(),
    )?)
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::warn!("api: {error:#}");
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("{error:#}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// Creates the API state shared by the HTTP router and the file watcher.
pub fn new(
    paths: &AppPaths,
    database: Arc<Database>,
    port: u16,
    write_lock: Arc<Mutex<()>>,
) -> ApiState {
    ApiState {
        paths: paths.clone(),
        database,
        port,
        write_lock,
    }
}
