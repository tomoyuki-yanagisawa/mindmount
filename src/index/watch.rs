//! Watch mounted directories, coalescing filesystem events into rescans.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::config::{AppPaths, Config};
use crate::db::sqlite::Database;
use crate::id::Snowflake;

const DEBOUNCE: Duration = Duration::from_millis(500);
const RECONCILE: Duration = Duration::from_secs(30);

pub async fn run(paths: &AppPaths) -> Result<()> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })?;
    watcher
        .watch(paths.config_dir(), RecursiveMode::NonRecursive)
        .context("cannot watch the Mindmount config directory")?;
    let mut watched: HashSet<PathBuf> = HashSet::new();
    let mut dirty = true;
    let mut interval = tokio::time::interval(RECONCILE);
    interval.tick().await;
    let mut config_interval = tokio::time::interval(Duration::from_secs(1));
    config_interval.tick().await;
    let mut config_snapshot = None;

    eprintln!("Watching mounted directories; press Ctrl-C to stop.");
    'watch: loop {
        if dirty {
            // Wait for editor atomic saves and Git operations to settle.
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break 'watch,
                    _ = tokio::time::sleep(DEBOUNCE) => break,
                    event = receiver.recv() => if event.is_none() {
                        anyhow::bail!("filesystem notification channel closed");
                    },
                }
            }
            match reconcile(paths, &mut watcher, &mut watched).await {
                Ok(snapshot) => {
                    config_snapshot = Some(snapshot);
                    dirty = false;
                }
                Err(error) => {
                    eprintln!("watch: retrying after error: {error:#}");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => break 'watch,
                        _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                    }
                }
            }
            continue;
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = interval.tick() => dirty = true,
            _ = config_interval.tick() => {
                if std::fs::read(paths.config_file()).ok() != config_snapshot {
                    dirty = true;
                }
            }
            event = receiver.recv() => {
                match event {
                    Some(Ok(event)) if !matches!(event.kind, EventKind::Access(_)) => {
                        dirty = event.paths.is_empty() || event.paths.iter().any(|path| {
                            if path == &paths.config_file() {
                                return true;
                            }
                            let internal = path == paths.config_dir()
                                || path.starts_with(paths.config_dir().join("db"))
                                || path == &paths.id_state_file()
                                || path == &paths.config_dir().join("mindmount.lock")
                                || (path.parent() == Some(paths.config_dir())
                                    && path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('.')));
                            !internal && watched.iter().any(|root| path.starts_with(root))
                        });
                    }
                    Some(Err(error)) => {
                        eprintln!("watch: filesystem notification error: {error}");
                        dirty = true;
                    }
                    None => anyhow::bail!("filesystem notification channel closed"),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn reconcile(
    paths: &AppPaths,
    watcher: &mut impl Watcher,
    watched: &mut HashSet<PathBuf>,
) -> Result<Vec<u8>> {
    // Lock only while reading config and updating the index. IDs must be
    // reloaded under this lock because other CLI processes may issue IDs.
    let _lock = paths.lock()?;
    let config = Config::load_or_create(&paths.config_file())?;
    let snapshot = std::fs::read(paths.config_file())?;
    let desired: HashSet<PathBuf> = config
        .mounts
        .iter()
        .map(|mount| mount.directory_path.clone())
        .collect();
    for root in watched.drain() {
        let _ = watcher.unwatch(&root);
    }
    for root in desired {
        match watcher.watch(&root, RecursiveMode::Recursive) {
            Ok(()) => {
                watched.insert(root);
            }
            Err(error) => eprintln!("watch: cannot watch {}: {error}", root.display()),
        }
    }
    let database = Database::open(&paths.database_file()).await?;
    let mut ids = Snowflake::persistent(config.node_id, &paths.id_state_file())?;
    crate::app::reindex(&config, &database, &mut ids).await?;
    Ok(snapshot)
}
