//! MCP server exposing the local Markdown corpus.

use std::sync::Arc;

use rmcp::{
    handler::server::{
        router::tool::ToolRouter,
        tool::ToolCallContext,
        wrapper::{Json, Parameters},
    },
    model::{
        CallToolRequestParams, CallToolResponse, ListResourcesResult, ListToolsResult,
        PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig,
    },
    schemars,
    service::RequestContext,
    tool, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::{AppPaths, Config};
use crate::core::document::Document;
use crate::db::queries::LinkRow;
use crate::db::Database;
use crate::usecase::{
    document::{self, DocumentTarget},
    index as index_ops, knowledge,
};

#[derive(Clone)]
pub struct Mindmount {
    paths: AppPaths,
    database: Arc<Database>,
    tool_router: ToolRouter<Self>,
}

impl Mindmount {
    pub(crate) fn from_shared(paths: AppPaths, database: Arc<Database>) -> Self {
        Self {
            paths,
            database,
            tool_router: Self::tool_router(),
        }
    }

    fn config(&self) -> Result<Config, ErrorData> {
        Config::load_or_create(&self.paths.config_file()).map_err(internal)
    }

    fn database(&self) -> &Database {
        self.database.as_ref()
    }
}

pub fn http_service(
    paths: AppPaths,
    database: Arc<Database>,
) -> StreamableHttpService<Mindmount, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(Mindmount::from_shared(paths.clone(), database.clone())),
        Default::default(),
        StreamableHttpServerConfig::default(),
    )
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListArgs {
    #[schemars(description = "Only list documents in this mount.")]
    mount_id: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    #[schemars(description = "Text or regular expression to search for.")]
    query: String,
    #[schemars(description = "Only search within this mount.")]
    mount_id: Option<i64>,
    #[schemars(description = "Only search files whose relative path contains this text.")]
    filename: Option<String>,
    #[schemars(description = "Treat query as a regular expression.")]
    regex: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadArgs {
    #[schemars(description = "Indexed document id. Takes precedence over path.")]
    document_id: Option<i64>,
    #[schemars(description = "Mount id when reading by path.")]
    mount_id: Option<i64>,
    #[schemars(description = "Path relative to the mount root when reading by path.")]
    path: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LinksArgs {
    #[schemars(description = "Indexed document id.")]
    document_id: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MetadataGetArgs {
    #[schemars(description = "Indexed document id.")]
    document_id: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MetadataSetArgs {
    #[schemars(description = "Indexed document id.")]
    document_id: i64,
    #[schemars(description = "Metadata key.")]
    key: String,
    #[schemars(description = "Metadata value as JSON.")]
    value: Value,
    #[schemars(description = "Author of the update. Defaults to mcp.")]
    updated_by: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LinkAddArgs {
    #[schemars(description = "Source document id.")]
    source_document_id: i64,
    #[schemars(description = "Target document id, when known.")]
    target_document_id: Option<i64>,
    #[schemars(description = "Target URI when the target document is not indexed.")]
    target_uri: Option<String>,
    #[schemars(description = "references, related_to, depends_on, derived_from, or supersedes.")]
    relation_type: String,
    #[schemars(description = "Optional human-readable description.")]
    description: Option<String>,
    #[schemars(description = "Optional provenance for the relation.")]
    provenance: Option<String>,
    #[schemars(description = "proposed or confirmed. Defaults to proposed.")]
    status: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LinkDeleteArgs {
    #[schemars(description = "Knowledge link id.")]
    link_id: i64,
}

impl ReadArgs {
    fn target(&self) -> Result<DocumentTarget, ErrorData> {
        target(self.document_id, self.mount_id, self.path.clone())
    }
}

fn target(
    document_id: Option<i64>,
    mount_id: Option<i64>,
    path: Option<String>,
) -> Result<DocumentTarget, ErrorData> {
    if let Some(id) = document_id {
        return Ok(DocumentTarget::Id(id));
    }
    match (mount_id, path) {
        (Some(mount_id), Some(path)) => Ok(DocumentTarget::Path { mount_id, path }),
        _ => Err(ErrorData::invalid_params(
            "provide document_id, or mount_id and path",
            None,
        )),
    }
}

#[tool_router]
impl Mindmount {
    #[tool(description = "List registered mounts with document counts.")]
    async fn mounts(&self) -> Result<Json<Value>, ErrorData> {
        let config = self.config()?;
        let counts = self.database().document_counts().await.map_err(internal)?;
        let mounts: Vec<Value> = config
            .mounts
            .iter()
            .map(|mount| {
                json!({
                    "id": mount.id,
                    "name": mount.name,
                    "project_path": mount.project_path.display().to_string(),
                    "directory_path": mount.directory_path.display().to_string(),
                    "documents": counts.get(&mount.id).copied().unwrap_or(0),
                })
            })
            .collect();
        Ok(Json(Value::Array(mounts)))
    }

    #[tool(description = "List indexed documents, optionally within one mount.")]
    async fn list(&self, Parameters(args): Parameters<ListArgs>) -> Result<Json<Value>, ErrorData> {
        let config = self.config()?;
        let documents = index_ops::pages(&config, self.database(), args.mount_id)
            .await
            .map_err(internal)?;
        Ok(Json(json!(documents
            .iter()
            .map(document_json)
            .collect::<Vec<_>>())))
    }

    #[tool(description = "Search Markdown by literal text and return matching lines.")]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let config = self.config()?;
        let hits = index_ops::search(
            &config,
            self.database(),
            &args.query,
            args.mount_id,
            args.filename.as_deref(),
            args.regex.unwrap_or(false),
        )
        .await
        .map_err(internal)?;
        Ok(Json(json!(hits
            .iter()
            .map(|hit| json!({
                "document_id": hit.document_id,
                "mount_id": hit.mount_id,
                "mount": hit.mount_name,
                "path": hit.relative_path,
                "line": hit.line,
                "text": hit.text,
                "before": hit.before,
                "after": hit.after,
            }))
            .collect::<Vec<_>>())))
    }

    #[tool(description = "Read a Markdown page by document_id or by mount_id and path.")]
    async fn read(&self, Parameters(args): Parameters<ReadArgs>) -> Result<Json<Value>, ErrorData> {
        let config = self.config()?;
        let result = document::read(&config, self.database(), args.target()?)
            .await
            .map_err(internal)?;
        Ok(Json(read_json(&result)))
    }

    #[tool(description = "List links from a page.")]
    async fn links(
        &self,
        Parameters(args): Parameters<LinksArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let config = self.config()?;
        let links = index_ops::links(&config, self.database(), args.document_id, false)
            .await
            .map_err(internal)?;
        Ok(Json(json!(links.iter().map(link_json).collect::<Vec<_>>())))
    }

    #[tool(description = "List pages linking to a page.")]
    async fn backlinks(
        &self,
        Parameters(args): Parameters<LinksArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let config = self.config()?;
        let links = index_ops::links(&config, self.database(), args.document_id, true)
            .await
            .map_err(internal)?;
        Ok(Json(json!(links.iter().map(link_json).collect::<Vec<_>>())))
    }

    #[tool(description = "Get DB-owned metadata for a page.")]
    async fn metadata_get(
        &self,
        Parameters(args): Parameters<MetadataGetArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let entries = knowledge::metadata(self.database(), args.document_id)
            .await
            .map_err(internal)?;
        Ok(Json(json!(entries
            .iter()
            .map(|entry| json!({
                "key": entry.key,
                "value": serde_json::from_str::<Value>(&entry.value_json)
                    .unwrap_or_else(|_| Value::String(entry.value_json.clone())),
                "updated_by": entry.updated_by,
                "updated_at": entry.updated_at.to_rfc3339(),
            }))
            .collect::<Vec<_>>())))
    }

    #[tool(description = "Set a DB-owned metadata value for a page.")]
    async fn metadata_set(
        &self,
        Parameters(args): Parameters<MetadataSetArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        knowledge::set_metadata(
            self.database(),
            args.document_id,
            &args.key,
            &args.value,
            args.updated_by.as_deref().unwrap_or("mcp"),
        )
        .await
        .map_err(internal)?;
        Ok(Json(json!({ "ok": true })))
    }

    #[tool(description = "Add a Knowledge Link between pages.")]
    async fn link_add(
        &self,
        Parameters(args): Parameters<LinkAddArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let id = knowledge::add_link(
            self.database(),
            args.source_document_id,
            args.target_document_id,
            args.target_uri.as_deref(),
            &args.relation_type,
            args.description.as_deref(),
            args.provenance.as_deref(),
            "mcp",
            args.status.as_deref().unwrap_or("proposed"),
        )
        .await
        .map_err(internal)?;
        Ok(Json(json!({ "id": id })))
    }

    #[tool(description = "Delete a Knowledge Link by id.")]
    async fn link_delete(
        &self,
        Parameters(args): Parameters<LinkDeleteArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let deleted = knowledge::delete_link(self.database(), args.link_id)
            .await
            .map_err(internal)?;
        Ok(Json(json!({ "deleted": deleted })))
    }
}

impl ServerHandler for Mindmount {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(
            "Mindmount exposes local Markdown. Read a page before updating it and pass the returned content_hash as expected_hash.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tool_router.list_all()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.tool_router
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let config = self.config()?;
        let documents = self
            .database()
            .list_documents(None)
            .await
            .map_err(internal)?;
        let resources = documents
            .iter()
            .map(|document| {
                let mount = config.find_by_id(document.mount_id);
                let description = match mount {
                    Some(mount) => format!("{}: {}", mount.name, document.relative_path),
                    None => document.relative_path.clone(),
                };
                Resource::new(
                    resource_uri(document.mount_id, document.id),
                    document.title.clone(),
                )
                .with_description(description)
                .with_mime_type("text/markdown")
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (mount_id, document_id) = parse_resource_uri(&request.uri)
            .ok_or_else(|| ErrorData::invalid_params("invalid resource URI", None))?;
        let config = self.config()?;
        let result = document::read(&config, self.database(), DocumentTarget::Id(document_id))
            .await
            .map_err(internal)?;
        if result.mount.id != mount_id {
            return Err(ErrorData::invalid_params(
                "resource does not belong to that mount",
                None,
            ));
        }
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(result.content, request.uri).with_mime_type("text/markdown")
        ])
        .into())
    }
}

fn internal(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

fn document_json(document: &Document) -> Value {
    json!({
        "id": document.id,
        "mount_id": document.mount_id,
        "path": document.relative_path,
        "title": document.title,
        "content_hash": document.content_hash,
        "modified_at": document.modified_at.to_rfc3339(),
    })
}

fn link_json(link: &LinkRow) -> Value {
    json!({
        "source_id": link.source_id,
        "source_path": link.source_path,
        "target_id": link.target_id,
        "target": link.target,
        "anchor": link.anchor,
        "kind": link.kind,
        "origin": link.origin,
    })
}

fn read_json(result: &document::ReadResult) -> Value {
    let document = result.document.as_ref();
    json!({
        "id": document.map(|document| document.id),
        "mount_id": result.mount.id,
        "path": result.relative_path,
        "title": document.map(|document| document.title.clone()),
        "content": result.content,
        "content_hash": result.content_hash,
    })
}

fn resource_uri(mount_id: i64, document_id: i64) -> String {
    format!("mindmount://{mount_id}/documents/{document_id}")
}

fn parse_resource_uri(uri: &str) -> Option<(i64, i64)> {
    let rest = uri.strip_prefix("mindmount://")?;
    let (mount_id, document_id) = rest.split_once("/documents/")?;
    Some((mount_id.parse().ok()?, document_id.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_the_expected_tools_without_write() {
        let tools = Mindmount::tool_router().list_all();
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        for expected in [
            "mounts",
            "list",
            "search",
            "read",
            "links",
            "backlinks",
            "metadata_get",
            "metadata_set",
            "link_add",
            "link_delete",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        assert!(!names.contains(&"write"));
    }
}
