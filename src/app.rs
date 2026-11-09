use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;

use crate::cli::{Cli, Command, IndexCommand, StorageCommand};
use crate::config::{AppPaths, Config, Mount};
use crate::db::sqlite::Database;
use crate::id::Snowflake;
use crate::index::indexer::{self, SyncReport};
use crate::storage::filesystem::MountRoot;

pub async fn run(cli: Cli) -> Result<()> {
    if cli.command.is_none() && cli.mount.project.is_none() && cli.mount.directory.is_none() {
        crate::cli::print_help();
        return Ok(());
    }

    let paths = AppPaths::from_env()?;
    paths.ensure_dirs()?;
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
            let result = register_mount(
                &mut config,
                &paths,
                &database,
                &mut ids,
                project,
                directory,
                cli.mount.name.clone(),
            )
            .await?;
            print_mount_result(&result);
            println!(
                "note: the read-only web viewer will listen on {}:{} starting in Phase 4",
                cli.server.host, cli.server.port
            );
        }
        Some(Command::Storage { command }) => match command {
            StorageCommand::Mount(options) => {
                let result = register_mount(
                    &mut config,
                    &paths,
                    &database,
                    &mut ids,
                    &options.project,
                    &options.directory,
                    options.name,
                )
                .await?;
                print_mount_result(&result);
            }
            StorageCommand::List => list_mounts(&config, &database).await?,
            StorageCommand::Unmount { mount_id } => {
                unmount(&mut config, &paths, &database, mount_id).await?
            }
        },
        Some(Command::Index { command }) => match command {
            IndexCommand::Rescan => reindex(&config, &database, &mut ids).await?,
            IndexCommand::Watch => unreachable!("watch is handled before acquiring the lock"),
            IndexCommand::Pages(options) => {
                list_pages(&config, &database, options.mount_id).await?
            }
            IndexCommand::Search { query, mount_id } => {
                search_pages(&config, &database, &query, mount_id).await?
            }
            IndexCommand::References { document_id } => {
                list_links(&config, &database, document_id, false).await?
            }
            IndexCommand::Backlinks { document_id } => {
                list_links(&config, &database, document_id, true).await?
            }
        },
        Some(Command::Serve(options)) => bail!(
            "`serve` (read-only web viewer) is not implemented yet (Phase 4); requested {}:{}",
            options.host,
            options.port
        ),
        Some(Command::Mcp) => bail!("`mcp` is not implemented yet (Phase 3)"),
    }

    Ok(())
}

struct RegisteredMount {
    mount: Mount,
    report: SyncReport,
    already_registered: bool,
}

#[allow(clippy::too_many_arguments)]
async fn register_mount(
    config: &mut Config,
    paths: &AppPaths,
    database: &Database,
    ids: &mut Snowflake,
    project: &Path,
    directory: &Path,
    name: Option<String>,
) -> Result<RegisteredMount> {
    let project_path = canonical_directory(project)?;
    let requested = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        project_path.join(directory)
    };
    let directory_path = canonical_directory(&requested)?;

    if !directory_path.starts_with(&project_path) {
        bail!(
            "mount directory {} must be inside the project {}",
            directory_path.display(),
            project_path.display()
        );
    }

    if let Some(existing) = config.find_by_directory(&directory_path) {
        let existing = existing.clone();
        database.sync_mounts(&config.mounts).await?;
        let root = MountRoot::new(&existing.directory_path).with_context(|| {
            format!(
                "mount directory is no longer accessible: {}",
                existing.directory_path.display()
            )
        })?;
        let report = indexer::sync_mount(database, existing.id, &root, ids)
            .await
            .with_context(|| format!("cannot index mount {}", existing.id))?;
        return Ok(RegisteredMount {
            mount: existing,
            report,
            already_registered: true,
        });
    }

    for other in &config.mounts {
        if directory_path.starts_with(&other.directory_path)
            || other.directory_path.starts_with(&directory_path)
        {
            bail!(
                "mount directory {} overlaps with existing mount \"{}\" ({})",
                directory_path.display(),
                other.name,
                other.directory_path.display()
            );
        }
    }

    let name = name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| default_name(&directory_path));

    let mount = Mount {
        id: ids.next_id().context("cannot generate a mount id")?,
        name,
        project_path,
        directory_path,
        created_at: Utc::now(),
    };

    config.mounts.push(mount.clone());
    if let Err(error) = config.validate() {
        config.mounts.pop();
        return Err(error);
    }

    config.save(&paths.config_file())?;
    database.sync_mounts(&config.mounts).await?;

    let root = MountRoot::new(&mount.directory_path)?;
    let report = indexer::sync_mount(database, mount.id, &root, ids)
        .await
        .with_context(|| {
            format!(
                "mount {} was registered but indexing failed; run `mindmount index rescan`",
                mount.id
            )
        })?;

    Ok(RegisteredMount {
        mount,
        report,
        already_registered: false,
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("cannot access directory {}", path.display()))?;
    if !canonical.is_dir() {
        bail!("not a directory: {}", path.display());
    }
    Ok(canonical)
}

fn default_name(directory: &Path) -> String {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("mount")
        .to_string()
}

fn print_mount_result(result: &RegisteredMount) {
    if result.already_registered {
        println!(
            "Mounted \"{}\" ({}) [already registered]",
            result.mount.name, result.mount.id
        );
    } else {
        println!("Mounted \"{}\" ({})", result.mount.name, result.mount.id);
    }
    println!("  project:   {}", result.mount.project_path.display());
    println!("  directory: {}", result.mount.directory_path.display());
    println!("  indexed:   {}", result.report.summary());
}

async fn list_mounts(config: &Config, database: &Database) -> Result<()> {
    database.sync_mounts(&config.mounts).await?;

    if config.mounts.is_empty() {
        println!("No mounts registered.");
        return Ok(());
    }

    let counts = database.document_counts().await?;
    println!("ID\tNAME\tDOCUMENTS\tDIRECTORY");
    for mount in &config.mounts {
        let count = counts.get(&mount.id).copied().unwrap_or(0);
        println!(
            "{}\t{}\t{}\t{}",
            mount.id,
            mount.name,
            count,
            mount.directory_path.display()
        );
    }
    Ok(())
}

async fn list_pages(config: &Config, database: &Database, mount_id: Option<i64>) -> Result<()> {
    database.sync_mounts(&config.mounts).await?;

    if let Some(mount_id) = mount_id {
        if config.find_by_id(mount_id).is_none() {
            bail!("mount {} is not registered", mount_id);
        }
    }

    let mut documents = database.list_documents(mount_id).await?;
    if documents.is_empty() {
        println!("No pages indexed.");
        return Ok(());
    }
    documents.sort_by(|left, right| {
        left.mount_id
            .cmp(&right.mount_id)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });

    let mount_names: HashMap<i64, &str> = config
        .mounts
        .iter()
        .map(|mount| (mount.id, mount.name.as_str()))
        .collect();

    println!("ID\tMOUNT\tPATH\tTITLE");
    for document in documents {
        let mount_name = mount_names
            .get(&document.mount_id)
            .copied()
            .unwrap_or("(unknown)");
        println!(
            "{}\t{}\t{}\t{}",
            document.id, mount_name, document.relative_path, document.title
        );
    }
    Ok(())
}

async fn search_pages(
    config: &Config,
    database: &Database,
    query: &str,
    mount_id: Option<i64>,
) -> Result<()> {
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
    let results = database.search_documents(query, mount_id).await?;
    if results.is_empty() {
        println!("No matching pages.");
        return Ok(());
    }
    println!("ID\tMOUNT\tPATH\tTITLE");
    for document in results {
        let name = config
            .find_by_id(document.mount_id)
            .map(|mount| mount.name.as_str())
            .unwrap_or("(unknown)");
        println!(
            "{}\t{}\t{}\t{}",
            document.id, name, document.relative_path, document.title
        );
    }
    Ok(())
}

async fn list_links(
    config: &Config,
    database: &Database,
    document_id: i64,
    backlinks: bool,
) -> Result<()> {
    database.sync_mounts(&config.mounts).await?;
    if !database.document_exists(document_id).await? {
        bail!("page {} is not indexed", document_id);
    }
    let missing = database.unindexed_search_count(None).await?;
    if missing > 0 {
        bail!("{missing} pages need indexing; run `mindmount index rescan`");
    }
    let links = database.links(document_id, backlinks).await?;
    if links.is_empty() {
        println!("No links found.");
        return Ok(());
    }
    println!("SOURCE_ID\tSOURCE_PATH\tTARGET_ID\tTARGET\tKIND");
    for link in links {
        let target = match link.anchor {
            Some(anchor) => format!("{}#{anchor}", link.target),
            None => link.target,
        };
        let id = link
            .target_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{}\t{}\t{}\t{}\t{}",
            link.source_id, link.source_path, id, target, link.kind
        );
    }
    Ok(())
}

async fn unmount(
    config: &mut Config,
    paths: &AppPaths,
    database: &Database,
    mount_id: i64,
) -> Result<()> {
    let removed = config
        .remove_mount(mount_id)
        .with_context(|| format!("mount {} is not registered", mount_id))?;

    config.save(&paths.config_file())?;
    database.sync_mounts(&config.mounts).await?;

    println!("Unmounted \"{}\" ({})", removed.name, removed.id);
    println!("Markdown files were left untouched.");
    Ok(())
}

pub(crate) async fn reindex(
    config: &Config,
    database: &Database,
    ids: &mut Snowflake,
) -> Result<()> {
    database.sync_mounts(&config.mounts).await?;

    if config.mounts.is_empty() {
        println!("No mounts registered.");
        return Ok(());
    }

    let mut failures = 0;
    for mount in &config.mounts {
        let root = match MountRoot::new(&mount.directory_path) {
            Ok(root) => root,
            Err(error) => {
                failures += 1;
                eprintln!(
                    "error: cannot reindex \"{}\" ({}): {error:#}",
                    mount.name, mount.id
                );
                continue;
            }
        };

        match indexer::sync_mount(database, mount.id, &root, ids).await {
            Ok(report) => println!(
                "Reindexed \"{}\" ({}): {}",
                mount.name,
                mount.id,
                report.summary()
            ),
            Err(error) => {
                failures += 1;
                eprintln!(
                    "error: cannot reindex \"{}\" ({}): {error:#}",
                    mount.name, mount.id
                );
            }
        }
    }

    if failures > 0 {
        bail!("reindex completed with {failures} error(s)");
    }
    Ok(())
}
