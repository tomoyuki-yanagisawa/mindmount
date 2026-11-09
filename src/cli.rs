use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};

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
    /// Start the read-only web viewer (Phase 4).
    Serve(ServerOptions),
    /// Start the MCP stdio server (Phase 3).
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
