//! CLI adapter: command definitions, dispatch, and rendering.

pub mod output;

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde_json::Value;

use crate::config::{AppPaths, Config};
use crate::core::id::Snowflake;
use crate::daemon;
use crate::db::Database;
use crate::usecase::{index as index_ops, knowledge, mount};

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
    /// Start the daemon in the foreground (web viewer and MCP HTTP).
    Serve(ServerOptions),
    /// Start the optional MCP stdio server.
    Mcp,
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
    /// List registered mounts.
    List,
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
    /// Rebuild the derived index for every registered mount.
    Rescan,
    /// Watch mounted directories and update the index until interrupted.
    Watch,
    /// List indexed pages.
    Pages(PagesOptions),
    /// Search indexed Markdown by literal text.
    Search {
        /// Text to search for.
        query: String,
        /// Only search within this mount.
        #[arg(short = 'm', long = "mount")]
        mount_id: Option<i64>,
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
    paths.ensure_dirs()?;
    if matches!(cli.command, Some(Command::Mcp)) {
        return crate::mcp::run(&paths).await;
    }
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
    if let Some(Command::Serve(options)) = &cli.command {
        return daemon::run(&paths, &options.host, options.port).await;
    }
    if matches!(
        cli.command,
        Some(Command::Index {
            command: IndexCommand::Watch
        })
    ) {
        return crate::index::watch::run(&paths).await;
    }
    let _lock = paths.lock()?;
    let mut config = Config::load_or_create(&paths.config_file())?;
    let database = Database::open(&paths.database_file()).await?;
    let mut ids = Snowflake::persistent(config.node_id, &paths.id_state_file())?;

    match cli.command {
        None => {
            let (Some(project), Some(directory)) = (&cli.mount.project, &cli.mount.directory)
            else {
                bail!("both --project and --directory are required to mount a directory");
            };
            let result = mount::register(
                &mut config,
                &paths,
                &database,
                &mut ids,
                project,
                directory,
                cli.mount.name.clone(),
            )
            .await?;
            output::mount_result(&result);
            println!(
                "note: run `mindmount serve --host {} --port {}` to open the read-only web viewer",
                cli.server.host, cli.server.port
            );
        }
        Some(Command::Mount { command }) => match command {
            MountCommand::Add(options) => {
                let result = mount::register(
                    &mut config,
                    &paths,
                    &database,
                    &mut ids,
                    &options.project,
                    &options.directory,
                    options.name,
                )
                .await?;
                output::mount_result(&result);
            }
            MountCommand::List => {
                let rows = mount::list(&config, &database).await?;
                output::mounts(&rows);
            }
            MountCommand::Remove { mount_id } => {
                let removed = mount::unmount(&mut config, &paths, &database, mount_id).await?;
                output::unmounted(&removed);
            }
            MountCommand::Sync { mount_id, all } => match (mount_id, all) {
                (_, true) => {
                    let outcome = index_ops::rescan(&config, &database, &mut ids).await?;
                    output::reindex(&outcome)?;
                }
                (Some(mount_id), false) => {
                    let report =
                        index_ops::resync_mount(&config, &database, &mut ids, mount_id).await?;
                    println!("Reindexed mount {mount_id}: {}", report.summary());
                }
                (None, false) => bail!("specify a mount id or --all"),
            },
        },
        Some(Command::Index { command }) => match command {
            IndexCommand::Rescan => {
                let outcome = index_ops::rescan(&config, &database, &mut ids).await?;
                output::reindex(&outcome)?;
            }
            IndexCommand::Watch => unreachable!("watch is handled before acquiring the lock"),
            IndexCommand::Pages(options) => {
                let documents = index_ops::pages(&config, &database, options.mount_id).await?;
                output::page_list(&config, &documents, "No pages indexed.");
            }
            IndexCommand::Search { query, mount_id } => {
                let hits = index_ops::search(&config, &database, &query, mount_id).await?;
                output::search_hits(&hits);
            }
            IndexCommand::References { document_id } => {
                let links = index_ops::links(&config, &database, document_id, false).await?;
                output::links(&links);
            }
            IndexCommand::Backlinks { document_id } => {
                let links = index_ops::links(&config, &database, document_id, true).await?;
                output::links(&links);
            }
        },
        Some(Command::Metadata { command }) => match command {
            MetadataCommand::Get { document_id } => {
                let entries = knowledge::metadata(&database, document_id).await?;
                output::metadata(&entries);
            }
            MetadataCommand::Set {
                document_id,
                key,
                value,
            } => {
                let value: Value = serde_json::from_str(&value).unwrap_or(Value::String(value));
                knowledge::set_metadata(&database, document_id, &key, &value, "cli").await?;
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
                let id = knowledge::add_link(
                    &database,
                    source_document_id,
                    target,
                    uri.as_deref(),
                    &relation,
                    description.as_deref(),
                    provenance.as_deref(),
                    "cli",
                    &status,
                )
                .await?;
                println!("Added knowledge link {id}");
            }
            LinkCommand::Delete { link_id } => {
                let deleted = knowledge::delete_link(&database, link_id).await?;
                println!(
                    "{} knowledge link {link_id}",
                    if deleted { "Deleted" } else { "No such" }
                );
            }
        },
        Some(Command::Backup { path }) => {
            database.backup(&path).await?;
            println!("Backed up to {}", path.display());
        }
        Some(Command::Version) => println!("mindmount {}", env!("CARGO_PKG_VERSION")),
        Some(Command::Daemon { .. }) => unreachable!("daemon is handled before acquiring the lock"),
        Some(Command::Serve(_)) => unreachable!("serve is handled before acquiring the lock"),
        Some(Command::Mcp) => unreachable!("mcp is handled before acquiring the lock"),
    }

    Ok(())
}
