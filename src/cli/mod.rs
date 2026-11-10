//! CLI adapter: command definitions, dispatch, and rendering.

pub mod output;

use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::config::{AppPaths, Config};
use crate::core::id::Snowflake;
use crate::db::Database;
use crate::usecase::{index as index_ops, mount};

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
    Storage {
        #[command(subcommand)]
        command: StorageCommand,
    },
    /// Manage the derived index.
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Start the read-only web viewer.
    Serve(ServerOptions),
    /// Start the MCP stdio server.
    Mcp,
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// Register a directory and build its derived index.
    Mount(MountOptions),
    /// List registered mounts.
    List,
    /// Remove a mount from the registry. Markdown files are never deleted.
    Unmount {
        /// Internal mount id shown by `mindmount storage list`.
        mount_id: i64,
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
    if let Some(Command::Serve(options)) = &cli.command {
        return crate::web::run(&paths, &options.host, options.port).await;
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
        Some(Command::Storage { command }) => match command {
            StorageCommand::Mount(options) => {
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
            StorageCommand::List => {
                let rows = mount::list(&config, &database).await?;
                output::mounts(&rows);
            }
            StorageCommand::Unmount { mount_id } => {
                let removed = mount::unmount(&mut config, &paths, &database, mount_id).await?;
                output::unmounted(&removed);
            }
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
                let documents = index_ops::search(&config, &database, &query, mount_id).await?;
                output::page_list(&config, &documents, "No matching pages.");
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
        Some(Command::Serve(_)) => unreachable!("serve is handled before acquiring the lock"),
        Some(Command::Mcp) => unreachable!("mcp is handled before acquiring the lock"),
    }

    Ok(())
}
