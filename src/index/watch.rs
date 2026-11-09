//! Watch mounted directories, coalescing filesystem events into rescans.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use crate::config::{AppPaths, Config};
use crate::db::sqlite::Database;
use crate::id::Snowflake;
use crate::index::indexer;
use crate::storage::filesystem::MountRoot;

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
    let mut missing: HashMap<i64, HashMap<String, Instant>> = HashMap::new();

    eprintln!("Watching mounted directories; press Ctrl-C to stop.");
    'watch: loop {
        if dirty {
            // Wait for editor atomic saves and Git operations to settle.
            let deadline = tokio::time::sleep(DEBOUNCE);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break 'watch,
                    _ = &mut deadline => break,
                    event = receiver.recv() => match event {
                        Some(Ok(event)) if relevant(&event, paths, &watched) => {
                            deadline.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE);
                        }
                        Some(Err(error)) => {
                            eprintln!("watch: filesystem notification error: {error}");
                            deadline.as_mut().reset(tokio::time::Instant::now() + DEBOUNCE);
                        }
                        None => anyhow::bail!("filesystem notification channel closed"),
                        _ => {}
                    },
                }
            }
            match reconcile(paths, &mut watcher, &mut watched, &mut missing).await {
                Ok((snapshot, deferred)) => {
                    config_snapshot = Some(snapshot);
                    dirty = deferred;
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
                    Some(Ok(event)) => dirty = relevant(&event, paths, &watched),
                    Some(Err(error)) => {
                        eprintln!("watch: filesystem notification error: {error}");
                        dirty = true;
                    }
                    None => anyhow::bail!("filesystem notification channel closed"),
                }
            }
        }
    }
    Ok(())
}

fn relevant(event: &notify::Event, paths: &AppPaths, watched: &HashSet<PathBuf>) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.is_empty()
        || event.paths.iter().any(|path| {
            if path == &paths.config_file() {
                return true;
            }
            let internal = path == paths.config_dir()
                || path.starts_with(paths.config_dir().join("db"))
                || path == &paths.id_state_file()
                || path == &paths.config_dir().join("mindmount.lock")
                || (path.parent() == Some(paths.config_dir())
                    && path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.')));
            !internal && watched.iter().any(|root| path.starts_with(root))
        })
}

async fn reconcile(
    paths: &AppPaths,
    watcher: &mut impl Watcher,
    watched: &mut HashSet<PathBuf>,
    missing: &mut HashMap<i64, HashMap<String, Instant>>,
) -> Result<(Vec<u8>, bool)> {
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
    database.sync_mounts(&config.mounts).await?;
    missing.retain(|id, _| config.mounts.iter().any(|mount| mount.id == *id));
    let mut deferred = false;
    let mut errors = Vec::new();
    for mount in &config.mounts {
        let result = match MountRoot::new(&mount.directory_path) {
            Ok(root) => {
                indexer::sync_mount_watched(
                    &database,
                    mount.id,
                    &root,
                    &mut ids,
                    missing.entry(mount.id).or_default(),
                )
                .await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok((_, true)) => deferred = true,
            Ok((report, false)) => println!(
                "Reindexed \"{}\" ({}): {}",
                mount.name,
                mount.id,
                report.summary()
            ),
            Err(error) => errors.push(format!("{} ({}): {error:#}", mount.name, mount.id)),
        }
    }
    if !errors.is_empty() {
        anyhow::bail!("cannot reindex {}", errors.join("; "));
    }
    Ok((snapshot, deferred))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_access_and_internal_notifications() {
        let paths = AppPaths::new("/config/mindmount");
        let watched = HashSet::from([PathBuf::from("/notes")]);
        let access = notify::Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path(PathBuf::from("/notes/page.md"));
        let lock =
            notify::Event::new(EventKind::Any).add_path(paths.config_dir().join("mindmount.lock"));
        let changed = notify::Event::new(EventKind::Any).add_path(PathBuf::from("/notes/page.md"));
        assert!(!relevant(&access, &paths, &watched));
        assert!(!relevant(&lock, &paths, &watched));
        assert!(relevant(&changed, &paths, &watched));
    }
}
