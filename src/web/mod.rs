//! Read-only local web viewer.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use askama::Template;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

use crate::config::{AppPaths, Config};
use crate::db::queries::LinkRow;
use crate::db::Database;
use crate::usecase::{
    document::{self, DocumentTarget},
    index as index_ops, mount as mount_ops,
};

#[derive(Clone)]
pub struct AppState {
    paths: AppPaths,
    database: Arc<Database>,
    port: u16,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(mounts_page))
        .route("/mounts", get(mounts_page))
        .route("/page/{id}", get(page))
        .route("/search", get(search_page))
        .route("/static/style.css", get(style))
        .fallback(not_found)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, guard))
}

pub async fn run(paths: &AppPaths, host: &str, port: u16) -> Result<()> {
    if !is_loopback(host) {
        bail!("serve binds only to a loopback address (127.0.0.1 or ::1)");
    }

    let database = Database::open(&paths.database_file()).await?;
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("cannot bind {host}:{port}"))?;
    let port = listener.local_addr()?.port();
    let state = AppState {
        paths: paths.clone(),
        database: Arc::new(database),
        port,
    };
    let app = router(state);
    let display_host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    eprintln!("Mindmount viewer listening on http://{display_host}:{port}");
    axum::serve(listener, app)
        .await
        .context("the web server stopped unexpectedly")?;
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1")
}

async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(host) = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return (StatusCode::BAD_REQUEST, "missing Host header").into_response();
    };
    if !allowed_host(host, state.port) {
        return (StatusCode::FORBIDDEN, "unexpected Host").into_response();
    }
    if let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        if !origin_allowed(origin, host) {
            return (StatusCode::FORBIDDEN, "unexpected Origin").into_response();
        }
    }
    next.run(request).await
}

fn allowed_host(host: &str, port: u16) -> bool {
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|name| host == *name || host == format!("{name}:{port}"))
}

fn origin_allowed(origin: &str, host: &str) -> bool {
    origin.strip_prefix("http://") == Some(host)
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    mount: Option<i64>,
}

struct MountView {
    id: i64,
    name: String,
    documents: i64,
    directory: String,
}

#[derive(Template)]
#[template(path = "mounts.html")]
struct MountsTemplate {
    mounts: Vec<MountView>,
}

struct LinkView {
    linked: bool,
    url: String,
    label: String,
    kind: String,
}

struct MetadataView {
    key: String,
    value: String,
}

#[derive(Template)]
#[template(path = "page.html")]
struct PageTemplate {
    title: String,
    mount_name: String,
    path: String,
    content: String,
    references: Vec<LinkView>,
    backlinks: Vec<LinkView>,
    knowledge_references: Vec<LinkView>,
    knowledge_backlinks: Vec<LinkView>,
    metadata: Vec<MetadataView>,
}

struct MountOption {
    id: i64,
    name: String,
    selected: bool,
}

struct SearchHitView {
    linked: bool,
    url: String,
    mount: String,
    path: String,
    line: usize,
    text: String,
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchTemplate {
    query: String,
    mounts: Vec<MountOption>,
    results: Vec<SearchHitView>,
    searched: bool,
}

async fn mounts_page(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let config = load_config(&state)?;
    let rows = mount_ops::list(&config, &state.database)
        .await
        .map_err(AppError::internal)?;
    let mounts = rows
        .iter()
        .map(|row| MountView {
            id: row.mount.id,
            name: row.mount.name.clone(),
            documents: row.documents,
            directory: row.mount.directory_path.display().to_string(),
        })
        .collect();
    render(MountsTemplate { mounts })
}

async fn page(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let config = load_config(&state)?;
    let read = document::read(&config, &state.database, DocumentTarget::Id(id))
        .await
        .map_err(|error| AppError::not_found(format!("page {id}: {error:#}")))?;
    let references = state
        .database
        .links(id, false)
        .await
        .map_err(AppError::internal)?;
    let backlinks = state
        .database
        .links(id, true)
        .await
        .map_err(AppError::internal)?;
    let markdown_references: Vec<LinkRow> = references
        .iter()
        .filter(|link| link.origin == "markdown")
        .cloned()
        .collect();
    let knowledge_references: Vec<LinkRow> = references
        .iter()
        .filter(|link| link.origin == "knowledge")
        .cloned()
        .collect();
    let markdown_backlinks: Vec<LinkRow> = backlinks
        .iter()
        .filter(|link| link.origin == "markdown")
        .cloned()
        .collect();
    let knowledge_backlinks: Vec<LinkRow> = backlinks
        .iter()
        .filter(|link| link.origin == "knowledge")
        .cloned()
        .collect();
    let metadata = state
        .database
        .metadata(id)
        .await
        .map_err(AppError::internal)?
        .into_iter()
        .map(|entry| MetadataView {
            key: entry.key,
            value: entry.value_json,
        })
        .collect();

    let content = render_markdown(&read.content, &markdown_references);
    let title = read
        .document
        .as_ref()
        .map(|document| document.title.clone())
        .unwrap_or_else(|| read.relative_path.clone());

    render(PageTemplate {
        title,
        mount_name: read.mount.name.clone(),
        path: read.relative_path.clone(),
        content,
        references: link_views(&markdown_references, false),
        backlinks: link_views(&markdown_backlinks, true),
        knowledge_references: link_views(&knowledge_references, false),
        knowledge_backlinks: link_views(&knowledge_backlinks, true),
        metadata,
    })
}

async fn search_page(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Html<String>, AppError> {
    let config = load_config(&state)?;
    let text = query.q.unwrap_or_default();
    let searched = !text.trim().is_empty();
    let results = if searched {
        index_ops::search(&config, &state.database, &text, query.mount)
            .await
            .map_err(AppError::internal)?
    } else {
        Vec::new()
    };

    let mounts = config
        .mounts
        .iter()
        .map(|mount| MountOption {
            id: mount.id,
            name: mount.name.clone(),
            selected: query.mount == Some(mount.id),
        })
        .collect();
    let results = results
        .iter()
        .map(|hit| SearchHitView {
            linked: hit.document_id.is_some(),
            url: hit
                .document_id
                .map(|id| format!("/page/{id}"))
                .unwrap_or_default(),
            mount: hit.mount_name.clone(),
            path: hit.relative_path.clone(),
            line: hit.line,
            text: hit.text.clone(),
        })
        .collect();

    render(SearchTemplate {
        query: text,
        mounts,
        results,
        searched,
    })
}

async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("static/style.css"),
    )
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}

fn load_config(state: &AppState) -> Result<Config, AppError> {
    Config::load_or_create(&state.paths.config_file()).map_err(AppError::internal)
}

fn render(template: impl Template) -> Result<Html<String>, AppError> {
    template.render().map(Html).map_err(AppError::internal)
}

fn link_views(links: &[LinkRow], backlinks: bool) -> Vec<LinkView> {
    links
        .iter()
        .map(|link| {
            if backlinks {
                LinkView {
                    linked: true,
                    url: format!("/page/{}", link.source_id),
                    label: link.source_path.clone(),
                    kind: link.kind.clone(),
                }
            } else {
                let label = if link.origin == "knowledge" {
                    if link.target.is_empty() {
                        link.target_id
                            .map(|id| format!("#{id}"))
                            .unwrap_or_else(|| "(unresolved)".into())
                    } else {
                        link.target.clone()
                    }
                } else {
                    match &link.anchor {
                        Some(anchor) => format!("{}#{anchor}", link.target),
                        None => link.target.clone(),
                    }
                };
                match link.target_id {
                    Some(id) => LinkView {
                        linked: true,
                        url: format!("/page/{id}{}", anchor_suffix(&link.anchor)),
                        label,
                        kind: link.kind.clone(),
                    },
                    None => LinkView {
                        linked: false,
                        url: String::new(),
                        label,
                        kind: link.kind.clone(),
                    },
                }
            }
        })
        .collect()
}

fn render_markdown(content: &str, links: &[LinkRow]) -> String {
    let targets: HashMap<&str, i64> = links
        .iter()
        .filter_map(|link| link.target_id.map(|id| (link.target.as_str(), id)))
        .collect();
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut events = Vec::new();
    let mut code_block = 0;
    let mut link_depth = 0;
    for event in Parser::new_ext(content, options) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                code_block += 1;
                events.push(event);
            }
            Event::End(TagEnd::CodeBlock) => {
                code_block -= 1;
                events.push(event);
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                link_depth += 1;
                let (target, anchor) = match dest_url.split_once('#') {
                    Some((target, anchor)) => (target.to_string(), Some(anchor.to_string())),
                    None => (dest_url.to_string(), None),
                };
                let dest_url = match targets.get(target.as_str()).copied() {
                    Some(target_id) => match anchor {
                        Some(anchor) => format!("/page/{target_id}#{anchor}").into(),
                        None => format!("/page/{target_id}").into(),
                    },
                    None => dest_url,
                };
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }));
            }
            Event::End(TagEnd::Link) => {
                link_depth -= 1;
                events.push(event);
            }
            Event::Text(text) if code_block == 0 && link_depth == 0 => {
                events.extend(wiki_events(&text, &targets));
            }
            event => events.push(event),
        }
    }
    let mut html = String::new();
    html::push_html(&mut html, events.into_iter());
    ammonia::clean(&html)
}

fn wiki_events<'a>(text: &str, targets: &HashMap<&str, i64>) -> Vec<Event<'a>> {
    let mut events = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        if start > 0 {
            events.push(Event::Text(rest[..start].to_string().into()));
        }
        let Some(end) = rest[start + 2..].find("]]") else {
            events.push(Event::Text(rest[start..].to_string().into()));
            return events;
        };
        let raw = &rest[start + 2..start + 2 + end];
        let (target, label) = match raw.split_once('|') {
            Some((target, label)) => (target.trim(), label.trim()),
            None => (raw.trim(), raw.trim()),
        };
        let target = target.split('#').next().unwrap_or(target);
        match targets.get(target).copied() {
            Some(id) => events.push(Event::InlineHtml(
                format!("<a href=\"/page/{id}\">{}</a>", escape_html(label)).into(),
            )),
            None => events.push(Event::Text(format!("[[{raw}]]").into())),
        }
        rest = &rest[start + 2 + end + 2..];
    }
    if !rest.is_empty() {
        events.push(Event::Text(rest.to_string().into()));
    }
    events
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn anchor_suffix(anchor: &Option<String>) -> String {
    anchor
        .as_ref()
        .map(|anchor| format!("#{anchor}"))
        .unwrap_or_default()
}

struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!("web: {error:#}");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::*;
    use crate::config::Mount;
    use crate::core::id::Snowflake;
    use crate::index::indexer;
    use crate::storage::filesystem::MountRoot;

    struct Fixture {
        _temp: tempfile::TempDir,
        state: AppState,
    }

    async fn fixture() -> Fixture {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let notes = project.join("notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(
            notes.join("index.md"),
            "---\nid: 42\n---\n# Index\n\n[Other](other.md)\n\n[[Other]]\n",
        )
        .unwrap();
        fs::write(
            notes.join("other.md"),
            "---\nid: 43\ntitle: Other\n---\n# Other\n",
        )
        .unwrap();

        let paths = AppPaths::new(temp.path().join("config"));
        paths.ensure_dirs().unwrap();
        let mount = Mount {
            id: 1,
            name: "Notes".into(),
            project_path: project,
            directory_path: notes,
            created_at: Utc::now(),
        };
        let config = Config {
            node_id: 1,
            mounts: vec![mount.clone()],
        };
        config.save(&paths.config_file()).unwrap();
        let database = Database::open(&paths.database_file()).await.unwrap();
        database
            .sync_mounts(std::slice::from_ref(&mount))
            .await
            .unwrap();
        let root = MountRoot::new(&mount.directory_path).unwrap();
        let mut ids = Snowflake::new(0).unwrap();
        indexer::sync_mount(&database, 1, &root, &mut ids)
            .await
            .unwrap();

        Fixture {
            _temp: temp,
            state: AppState {
                paths,
                database: Arc::new(database),
                port: 3000,
            },
        }
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .uri(uri)
            .header("host", "127.0.0.1:3000")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn renders_mounts_pages_and_search() {
        let fixture = fixture().await;
        let app = router(fixture.state);

        let (status, body) = get(&app, "/mounts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Notes"), "{body}");

        let (status, body) = get(&app, "/page/42").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Index"), "{body}");
        assert!(body.matches("/page/43").count() >= 2, "{body}");

        let (status, body) = get(&app, "/search?q=Other").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Other"), "{body}");

        let (status, _) = get(&app, "/page/999").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rejects_foreign_hosts_and_origins() {
        let fixture = fixture().await;
        let app = router(fixture.state);

        let request = Request::builder()
            .uri("/mounts")
            .header("host", "evil.example")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let request = Request::builder()
            .uri("/mounts")
            .header("host", "127.0.0.1:3000")
            .header("origin", "http://evil.example")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let request = Request::builder()
            .uri("/mounts")
            .header("host", "127.0.0.1:3000")
            .header("origin", "http://127.0.0.1:3000")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
