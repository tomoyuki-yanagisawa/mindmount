//! CLI adapter: command definitions, dispatch, and rendering.

pub mod client;
pub mod output;

use std::path::{absolute, PathBuf};

use anyhow::{bail, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde_json::{json, Value};

use crate::config::AppPaths;
use crate::core::document::Document;
use crate::daemon;
use crate::db::queries::{LinkRow, MetadataEntry};
use crate::index::indexer::{SyncOutcome, SyncReport};
use crate::search::SearchHit;
use crate::usecase::mount::{MountResult, MountRow};

use self::client::Client;

/// Local-first Markdown knowledge server for humans and AI.
#[derive(Debug, Parser)]
#[command(name = "mindmount", version, about)]
pub struct Cli {
    /// Subcommand. Omit to mount the directory given by --project/--directory.
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub mount: RootMountOptions,

    #[command(flatten)]
    pub server: ServerOptions,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage mounted directories.
    Mount {
        #[command(subcommand)]
        command: MountCommand,
    },
    /// Manage the derived index.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Manage DB-owned metadata.
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    /// Manage Knowledge Links.
    Link {
        #[command(subcommand)]
        command: LinkCommand,
    },
    /// Manage the local daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Create a consistent SQLite backup.
    Backup {
        /// Destination path. Must not already exist.
        path: PathBuf,
    },
    /// Print the version.
    Version,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon in the background.
    Start(ServerOptions),
    /// Stop the background daemon.
    Stop,
    /// Restart the background daemon.
    Restart(ServerOptions),
    /// Report whether the background daemon is running.
    Status,
    /// Run the daemon in the foreground.
    Run(ServerOptions),
}

#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    /// Show metadata for a page.
    Get { document_id: i64 },
    /// Set a metadata value. VALUE is parsed as JSON, or stored as a string.
    Set {
        document_id: i64,
        key: String,
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum LinkCommand {
    /// Add a Knowledge Link.
    Add {
        source_document_id: i64,
        #[arg(long)]
        target: Option<i64>,
        #[arg(long)]
        uri: Option<String>,
        #[arg(long, default_value = "related_to")]
        relation: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        provenance: Option<String>,
        #[arg(long, default_value = "proposed")]
        status: String,
    },
    /// Delete a Knowledge Link by id.
    Delete { link_id: i64 },
}

#[derive(Debug, Subcommand)]
pub enum MountCommand {
    /// Register a directory and build its derived index.
    Add(MountOptions),
    /// List registered mounts, optionally including inactive ones.
    List {
        #[arg(long)]
        all: bool,
    },
    /// Remove a mount from the registry. Markdown files are never deleted.
    Remove {
        /// Internal mount id shown by `mindmount mount list`.
        mount_id: i64,
    },
    /// Resynchronize one mount, or all mounts with --all.
    Sync {
        mount_id: Option<i64>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    /// List indexed pages.
    Pages(PagesOptions),
    /// Search indexed Markdown by literal text.
    Search {
        /// Text or regular expression to search for.
        query: String,
        /// Only search within this mount.
        #[arg(short = 'm', long = "mount")]
        mount_id: Option<i64>,
        /// Only search files whose relative path contains this text.
        #[arg(long)]
        file: Option<String>,
        /// Treat the query as a regular expression.
        #[arg(long)]
        regex: bool,
    },
    /// List links from a page.
    References { document_id: i64 },
    /// List pages linking to a page.
    Backlinks { document_id: i64 },
}

/// Options for the root invocation (`mindmount -p ... -d ...`).
#[derive(Debug, Args, Clone)]
pub struct RootMountOptions {
    /// Project root.
    #[arg(short = 'p', long = "project", value_name = "PATH")]
    pub project: Option<PathBuf>,

    /// Markdown directory, relative to the project root.
    #[arg(short = 'd', long = "directory", value_name = "PATH")]
    pub directory: Option<PathBuf>,

    /// Display name. Defaults to the directory name.
    #[arg(short = 'n', long = "name", value_name = "NAME")]
    pub name: Option<String>,
}

/// Options for the `mount` subcommand.
#[derive(Debug, Args, Clone)]
pub struct MountOptions {
    /// Project root.
    #[arg(short = 'p', long = "project", value_name = "PATH")]
    pub project: PathBuf,

    /// Markdown directory, relative to the project root.
    #[arg(short = 'd', long = "directory", value_name = "PATH")]
    pub directory: PathBuf,

    /// Display name. Defaults to the directory name.
    #[arg(short = 'n', long = "name", value_name = "NAME")]
    pub name: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct ServerOptions {
    /// HTTP port.
    #[arg(long, default_value_t = 3000)]
    pub port: u16,

    /// Bind address.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,
}

#[derive(Debug, Args)]
pub struct PagesOptions {
    /// Only list pages from this mount.
    #[arg(short = 'm', long = "mount", value_name = "MOUNT_ID")]
    pub mount_id: Option<i64>,
}

pub fn print_help() {
    let mut command = Cli::command();
    let _ = command.print_help();
    println!();
}

pub async fn run(cli: Cli) -> Result<()> {
    if cli.command.is_none() && cli.mount.project.is_none() && cli.mount.directory.is_none() {
        print_help();
        return Ok(());
    }

    let paths = AppPaths::from_env()?;
    if let Some(Command::Daemon { command }) = &cli.command {
        return match command {
            DaemonCommand::Start(options) => {
                daemon::start(&paths, &options.host, options.port).await
            }
            DaemonCommand::Stop => daemon::stop(&paths).await,
            DaemonCommand::Restart(options) => {
                daemon::restart(&paths, &options.host, options.port).await
            }
            DaemonCommand::Status => daemon::status(&paths).await,
            DaemonCommand::Run(options) => daemon::run(&paths, &options.host, options.port).await,
        };
    }
    if matches!(cli.command, Some(Command::Version)) {
        println!("mindmount {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let client = Client::from_env(&paths)?;
    match cli.command {
        None => {
            let (Some(project), Some(directory)) = (&cli.mount.project, &cli.mount.directory)
            else {
                bail!("both --project and --directory are required to mount a directory");
            };
            let result = add_mount(&client, project, directory, cli.mount.name.clone()).await?;
            output::mount_result(&result);
            println!(
                "note: run `mindmount daemon start --host {} --port {}` to open the read-only web viewer",
                cli.server.host, cli.server.port
            );
        }
        Some(Command::Mount { command }) => match command {
            MountCommand::Add(options) => {
                let result =
                    add_mount(&client, &options.project, &options.directory, options.name).await?;
                output::mount_result(&result);
            }
            MountCommand::List { all } => {
                let rows: Vec<MountRow> = client
                    .get_query("/api/admin/mounts", vec![("all".into(), all.to_string())])
                    .await?;
                output::mounts(&rows);
            }
            MountCommand::Remove { mount_id } => {
                let removed = client
                    .delete::<crate::config::Mount>(&format!("/api/admin/mounts/{mount_id}"))
                    .await?;
                output::unmounted(&removed);
            }
            MountCommand::Sync { mount_id, all } => match (mount_id, all) {
                (_, true) => {
                    let outcome: SyncOutcome =
                        client.post("/api/admin/mounts/sync", &json!({})).await?;
                    output::reindex(&outcome)?;
                }
                (Some(mount_id), false) => {
                    let report: SyncReport = client
                        .post(&format!("/api/admin/mounts/{mount_id}/sync"), &json!({}))
                        .await?;
                    println!("Reindexed mount {mount_id}: {}", report.summary());
                }
                (None, false) => bail!("specify a mount id or --all"),
            },
        },
        Some(Command::Index { command }) => match command {
            IndexCommand::Pages(options) => {
                let documents: Vec<Document> = client
                    .get_query(
                        "/api/documents",
                        options
                            .mount_id
                            .map(|id| vec![("mount".into(), id.to_string())])
                            .unwrap_or_default(),
                    )
                    .await?;
                output::page_list(&documents, "No pages indexed.");
            }
            IndexCommand::Search {
                query,
                mount_id,
                file,
                regex,
            } => {
                let mut params = vec![("q".into(), query)];
                if let Some(id) = mount_id {
                    params.push(("mount".into(), id.to_string()));
                }
                if let Some(file) = file {
                    params.push(("file".into(), file));
                }
                if regex {
                    params.push(("regex".into(), "true".into()));
                }
                let hits: Vec<SearchHit> = client.get_query("/api/search", params).await?;
                output::search_hits(&hits);
            }
            IndexCommand::References { document_id } => {
                let links: Vec<LinkRow> = client
                    .get(&format!("/api/documents/{document_id}/links"))
                    .await?;
                output::links(&links);
            }
            IndexCommand::Backlinks { document_id } => {
                let links: Vec<LinkRow> = client
                    .get(&format!("/api/documents/{document_id}/backlinks"))
                    .await?;
                output::links(&links);
            }
        },
        Some(Command::Metadata { command }) => match command {
            MetadataCommand::Get { document_id } => {
                let entries: Vec<MetadataEntry> = client
                    .get(&format!("/api/documents/{document_id}/metadata"))
                    .await?;
                output::metadata(&entries);
            }
            MetadataCommand::Set {
                document_id,
                key,
                value,
            } => {
                let value: Value = serde_json::from_str(&value).unwrap_or(Value::String(value));
                client
                    .put::<_, Value>(
                        &format!("/api/documents/{document_id}/metadata/{key}"),
                        &json!({ "value": value }),
                    )
                    .await?;
                println!("Updated metadata \"{key}\" for page {document_id}");
            }
        },
        Some(Command::Link { command }) => match command {
            LinkCommand::Add {
                source_document_id,
                target,
                uri,
                relation,
                description,
                provenance,
                status,
            } => {
                let response: Value = client
                    .post(
                        "/api/knowledge-links",
                        &json!({
                            "source_document_id": source_document_id,
                            "target_document_id": target,
                            "target_uri": uri,
                            "relation_type": relation,
                            "description": description,
                            "provenance": provenance,
                            "status": status,
                        }),
                    )
                    .await?;
                println!("Added knowledge link {}", response["id"]);
            }
            LinkCommand::Delete { link_id } => {
                let response: Value = client
                    .delete(&format!("/api/knowledge-links/{link_id}"))
                    .await?;
                let deleted = response["deleted"].as_bool().unwrap_or(false);
                println!(
                    "{} knowledge link {link_id}",
                    if deleted { "Deleted" } else { "No such" }
                );
            }
        },
        Some(Command::Backup { path }) => {
            let path = absolute(&path)?;
            client
                .post::<_, Value>("/api/admin/backup", &json!({ "path": path }))
                .await?;
            println!("Backed up to {}", path.display());
        }
        Some(Command::Version) => unreachable!("version is handled before creating the client"),
        Some(Command::Daemon { .. }) => {
            unreachable!("daemon is handled before creating the client")
        }
    }

    Ok(())
}

async fn add_mount(
    client: &Client,
    project: &std::path::Path,
    directory: &std::path::Path,
    name: Option<String>,
) -> Result<MountResult> {
    client
        .post(
            "/api/admin/mounts",
            &json!({
                "project": absolute(project)?,
                "directory": directory,
                "name": name,
            }),
        )
        .await
}
