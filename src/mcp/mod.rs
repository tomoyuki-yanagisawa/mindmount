//! MCP stdio server exposing the local Markdown corpus.

use std::sync::Arc;

use anyhow::{Context, Result};
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
    transport::stdio,
    ErrorData, RoleServer, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::{AppPaths, Config};
use crate::core::document::Document;
use crate::db::queries::LinkRow;
use crate::db::Database;
use crate::usecase::{
    document::{self, DocumentTarget},
    index as index_ops,
};

#[derive(Clone)]
pub struct Mindmount {
    paths: AppPaths,
    database: Arc<Database>,
    tool_router: ToolRouter<Self>,
}

impl Mindmount {
    fn new(paths: AppPaths, database: Database) -> Self {
        Self {
            paths,
            database: Arc::new(database),
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

pub async fn run(paths: &AppPaths) -> Result<()> {
    let database = Database::open(&paths.database_file()).await?;
    let server = Mindmount::new(paths.clone(), database);
    let service = server
        .serve(stdio())
        .await
        .context("cannot start the MCP server")?;
    service
        .waiting()
        .await
        .context("the MCP server stopped unexpectedly")?;
    Ok(())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListArgs {
    #[schemars(description = "Only list documents in this mount.")]
    mount_id: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    #[schemars(description = "Literal text to search for.")]
    query: String,
    #[schemars(description = "Only search within this mount.")]
    mount_id: Option<i64>,
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
struct WriteArgs {
    #[schemars(description = "Indexed document id. Takes precedence over path.")]
    document_id: Option<i64>,
    #[schemars(description = "Mount id when writing by path.")]
    mount_id: Option<i64>,
    #[schemars(description = "Path relative to the mount root when writing by path.")]
    path: Option<String>,
    #[schemars(description = "Full Markdown content to write.")]
    content: String,
    #[schemars(
        description = "content_hash returned by read. Required to update an existing page; omit to create a new one."
    )]
    expected_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LinksArgs {
    #[schemars(description = "Indexed document id.")]
    document_id: i64,
}

impl ReadArgs {
    fn target(&self) -> Result<DocumentTarget, ErrorData> {
        target(self.document_id, self.mount_id, self.path.clone())
    }
}

impl WriteArgs {
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
        let hits = index_ops::search(&config, self.database(), &args.query, args.mount_id)
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

    #[tool(
        description = "Write a Markdown page. Updating requires expected_hash from read; creating omits it."
    )]
    async fn write(
        &self,
        Parameters(args): Parameters<WriteArgs>,
    ) -> Result<Json<Value>, ErrorData> {
        let result = document::write(
            &self.paths,
            self.database(),
            args.target()?,
            &args.content,
            args.expected_hash.as_deref(),
        )
        .await
        .map_err(internal)?;
        Ok(Json(json!({
            "id": result.document.id,
            "mount_id": result.mount.id,
            "path": result.document.relative_path,
            "content_hash": result.content_hash,
        })))
    }

    #[tool(description = "List links from a page.")]
    async fn references(
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
