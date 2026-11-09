//! Builds the derived document index for a mount.
//!
//! Markdown files are authoritative. This module scans them, reads their
//! metadata, and applies the resulting changes to SQLite in one transaction.
//! File watching arrives in Phase 2.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tracing::warn;

use crate::core::document::{content_hash, parse_document, Document};
use crate::db::sqlite::Database;
use crate::id::Snowflake;
use crate::storage::filesystem::MountRoot;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub errors: usize,
}

impl SyncReport {
    pub fn summary(&self) -> String {
        format!(
            "{} files, {} added, {} updated, {} removed, {} unchanged, {} errors",
            self.scanned, self.added, self.updated, self.removed, self.unchanged, self.errors
        )
    }
}

pub async fn sync_mount(
    database: &Database,
    mount_id: i64,
    root: &MountRoot,
    ids: &mut Snowflake,
) -> Result<SyncReport> {
    Ok(sync_mount_inner(database, mount_id, root, ids, None)
        .await?
        .0)
}

/// A watch rescan waits for missing paths to remain absent before deleting them.
pub(crate) async fn sync_mount_watched(
    database: &Database,
    mount_id: i64,
    root: &MountRoot,
    ids: &mut Snowflake,
    missing_since: &mut HashMap<String, Instant>,
) -> Result<(SyncReport, bool)> {
    sync_mount_inner(database, mount_id, root, ids, Some(missing_since)).await
}

async fn sync_mount_inner(
    database: &Database,
    mount_id: i64,
    root: &MountRoot,
    ids: &mut Snowflake,
    missing_since: Option<&mut HashMap<String, Instant>>,
) -> Result<(SyncReport, bool)> {
    let existing: HashMap<String, Document> = database
        .documents_for_mount(mount_id)
        .await?
        .into_iter()
        .map(|document| (document.relative_path.clone(), document))
        .collect();

    let files = root.scan_markdown()?;
    let mut report = SyncReport {
        scanned: files.len(),
        ..SyncReport::default()
    };
    let mut scanned_paths = HashSet::new();
    let mut upserts = Vec::new();
    let mut search_content = HashMap::new();
    let mut unchanged = Vec::new();

    for relative in &files {
        let Some(relative_path) = relative_path_string(relative) else {
            warn!(path = %relative.display(), "skipping a non-UTF-8 path");
            report.errors += 1;
            continue;
        };
        scanned_paths.insert(relative_path.clone());

        let content = match root.read_markdown(relative) {
            Ok(content) => content,
            Err(error) => {
                warn!(path = %relative_path, error = %error, "skipping an unreadable file");
                report.errors += 1;
                continue;
            }
        };

        let hash = content_hash(&content);
        if let Some(previous) = existing.get(&relative_path) {
            if previous.content_hash == hash {
                report.unchanged += 1;
                unchanged.push(previous.clone());
                continue;
            }
        }

        let parsed = parse_document(&content);
        let title = parsed.title(&fallback_title(relative));
        let id = match parsed.frontmatter.id {
            Some(id) => id,
            None => match existing.get(&relative_path) {
                Some(previous) => previous.id,
                None => ids.next_id().context("cannot generate a document id")?,
            },
        };

        let modified_at = root.modified_at(relative).unwrap_or_else(|error| {
            warn!(path = %relative_path, error = %error, "using the current time as mtime");
            Utc::now()
        });

        if existing.contains_key(&relative_path) {
            report.updated += 1;
        } else {
            report.added += 1;
        }

        upserts.push(Document {
            id,
            mount_id,
            relative_path,
            title,
            content_hash: hash,
            modified_at,
            indexed_at: Utc::now(),
        });
        search_content.insert(id, content);
    }

    let deletes: Vec<String> = existing
        .keys()
        .filter(|path| !scanned_paths.contains(*path))
        .cloned()
        .collect();
    report.removed = deletes.len();

    if let Some(missing_since) = missing_since {
        let now = Instant::now();
        missing_since.retain(|path, _| deletes.contains(path));
        let mut deferred = false;
        for path in &deletes {
            let since = missing_since.entry(path.clone()).or_insert(now);
            deferred |= now.duration_since(*since) < Duration::from_secs(2);
        }
        if deferred {
            // Do not partially update the mount while a page may reappear.
            return Ok((report, true));
        }
    }

    // Catch duplicate frontmatter IDs before touching the database so the
    // whole mount stays consistent.
    let mut owners: HashMap<i64, String> = HashMap::new();
    for document in unchanged.iter().chain(upserts.iter()) {
        if let Some(other_path) = owners.insert(document.id, document.relative_path.clone()) {
            bail!(
                "document id {} is used by both {} and {}",
                document.id,
                other_path,
                document.relative_path
            );
        }
    }

    database
        .apply_mount_sync_with_content(mount_id, &upserts, &deletes, &search_content)
        .await?;

    Ok((report, false))
}

fn relative_path_string(relative: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn fallback_title(relative: &Path) -> String {
    relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("Untitled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;
    use crate::config::Mount;

    struct Fixture {
        _temp: tempfile::TempDir,
        notes: std::path::PathBuf,
        database: Database,
        mount: Mount,
        ids: Snowflake,
    }

    async fn fixture() -> Fixture {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let notes = project.join("notes");
        fs::create_dir_all(&notes).unwrap();

        let database = Database::open(&temp.path().join("db.sqlite"))
            .await
            .unwrap();
        let mount = Mount {
            id: 1,
            name: "notes".to_string(),
            project_path: project,
            directory_path: notes.clone(),
            created_at: Utc::now(),
        };
        database
            .sync_mounts(std::slice::from_ref(&mount))
            .await
            .unwrap();

        Fixture {
            _temp: temp,
            notes,
            database,
            mount,
            ids: Snowflake::new(0).unwrap(),
        }
    }

    impl Fixture {
        async fn sync(&mut self) -> anyhow::Result<SyncReport> {
            let root = MountRoot::new(&self.notes).unwrap();
            sync_mount(&self.database, self.mount.id, &root, &mut self.ids).await
        }
    }

    #[tokio::test]
    async fn watch_defers_missing_files_but_eventually_deletes_them() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("page.md"), "# Page\n").unwrap();
        fixture.sync().await.unwrap();
        fs::remove_file(fixture.notes.join("page.md")).unwrap();
        let root = MountRoot::new(&fixture.notes).unwrap();
        let mut missing = HashMap::new();
        let (_, deferred) =
            sync_mount_watched(&fixture.database, 1, &root, &mut fixture.ids, &mut missing)
                .await
                .unwrap();
        assert!(deferred);
        assert_eq!(
            fixture.database.documents_for_mount(1).await.unwrap().len(),
            1
        );
        missing.insert("page.md".into(), Instant::now() - Duration::from_secs(3));
        let (_, deferred) =
            sync_mount_watched(&fixture.database, 1, &root, &mut fixture.ids, &mut missing)
                .await
                .unwrap();
        assert!(!deferred);
        assert!(fixture
            .database
            .documents_for_mount(1)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn adds_updates_and_deletes_documents_while_preserving_internal_ids() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("index.md"), "# Index\n").unwrap();
        fs::write(fixture.notes.join("old.md"), "# Old\n").unwrap();

        let report = fixture.sync().await.unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.added, 2);
        assert_eq!(report.updated, 0);

        let before = fixture.database.documents_for_mount(1).await.unwrap();
        let index_id = before
            .iter()
            .find(|document| document.relative_path == "index.md")
            .unwrap()
            .id;

        fs::write(fixture.notes.join("index.md"), "# Updated\n").unwrap();
        fs::remove_file(fixture.notes.join("old.md")).unwrap();
        fs::write(fixture.notes.join("new.md"), "# New\n").unwrap();

        let report = fixture.sync().await.unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.added, 1);
        assert_eq!(report.updated, 1);
        assert_eq!(report.removed, 1);
        assert_eq!(report.unchanged, 0);

        let after = fixture.database.documents_for_mount(1).await.unwrap();
        assert_eq!(after.len(), 2);
        let index = after
            .iter()
            .find(|document| document.relative_path == "index.md")
            .unwrap();
        assert_eq!(index.id, index_id);
        assert_eq!(index.title, "Updated");

        let report = fixture.sync().await.unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.unchanged, 2);
        assert_eq!(report.added + report.updated + report.removed, 0);
    }

    #[tokio::test]
    async fn refreshes_stale_titles_without_changing_ids() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("page.md"), "# **Title**\n").unwrap();
        fixture.sync().await.unwrap();
        let mut document = fixture
            .database
            .documents_for_mount(1)
            .await
            .unwrap()
            .remove(0);
        let id = document.id;
        document.title = "**Title**".into();
        document.content_hash.clear(); // The title migration marks old rows for refresh.
        fixture
            .database
            .apply_mount_sync(1, &[document], &[])
            .await
            .unwrap();

        let report = fixture.sync().await.unwrap();
        let document = fixture
            .database
            .documents_for_mount(1)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(report.updated, 1);
        assert_eq!(document.id, id);
        assert_eq!(document.title, "Title");
    }

    #[tokio::test]
    async fn renaming_a_document_with_frontmatter_id_preserves_its_id() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("old.md"), "---\nid: 42\n---\n# Page\n").unwrap();
        fixture.sync().await.unwrap();
        fs::rename(fixture.notes.join("old.md"), fixture.notes.join("new.md")).unwrap();
        fixture.sync().await.unwrap();
        let documents = fixture.database.documents_for_mount(1).await.unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].id, 42);
        assert_eq!(documents[0].relative_path, "new.md");
    }

    #[tokio::test]
    async fn moving_over_an_existing_page_keeps_the_moved_id() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("old.md"), "---\nid: 42\n---\n# Old\n").unwrap();
        fs::write(fixture.notes.join("new.md"), "---\nid: 43\n---\n# New\n").unwrap();
        fixture.sync().await.unwrap();
        fs::rename(fixture.notes.join("old.md"), fixture.notes.join("new.md")).unwrap();
        fixture.sync().await.unwrap();
        let documents = fixture.database.documents_for_mount(1).await.unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(
            (documents[0].id, documents[0].relative_path.as_str()),
            (42, "new.md")
        );
    }

    #[tokio::test]
    async fn swapping_pages_with_frontmatter_ids_preserves_their_ids() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("a.md"), "---\nid: 42\n---\n").unwrap();
        fs::write(fixture.notes.join("b.md"), "---\nid: 43\n---\n").unwrap();
        fixture.sync().await.unwrap();
        fs::rename(fixture.notes.join("a.md"), fixture.notes.join("temp")).unwrap();
        fs::rename(fixture.notes.join("b.md"), fixture.notes.join("a.md")).unwrap();
        fs::rename(fixture.notes.join("temp"), fixture.notes.join("b.md")).unwrap();
        fixture.sync().await.unwrap();
        let docs = fixture.database.documents_for_mount(1).await.unwrap();
        assert_eq!((docs[0].id, docs[1].id), (43, 42));
    }

    #[tokio::test]
    async fn uses_frontmatter_ids_and_titles() {
        let mut fixture = fixture().await;
        fs::write(
            fixture.notes.join("page.md"),
            "---\nid: 42\ntitle: Declared title\n---\n# Ignored\n",
        )
        .unwrap();

        fixture.sync().await.unwrap();

        let documents = fixture.database.documents_for_mount(1).await.unwrap();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].id, 42);
        assert_eq!(documents[0].title, "Declared title");
    }

    #[tokio::test]
    async fn rejects_duplicate_document_ids() {
        let mut fixture = fixture().await;
        fs::write(fixture.notes.join("one.md"), "---\nid: 42\n---\n").unwrap();
        fs::write(fixture.notes.join("two.md"), "---\nid: 42\n---\n").unwrap();

        let error = fixture.sync().await.unwrap_err().to_string();
        assert!(error.contains("used by both"), "{error}");
        assert!(fixture
            .database
            .documents_for_mount(1)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn rebuilds_the_index_from_files_in_a_fresh_database() {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let notes = project.join("notes");
        fs::create_dir_all(&notes).unwrap();
        fs::write(notes.join("index.md"), "# Index\n").unwrap();

        let mount = Mount {
            id: 1,
            name: "notes".to_string(),
            project_path: project,
            directory_path: notes.clone(),
            created_at: Utc::now(),
        };
        let root = MountRoot::new(&notes).unwrap();
        let mut ids = Snowflake::new(0).unwrap();

        let first = Database::open(&temp.path().join("first.sqlite"))
            .await
            .unwrap();
        first
            .sync_mounts(std::slice::from_ref(&mount))
            .await
            .unwrap();
        sync_mount(&first, 1, &root, &mut ids).await.unwrap();
        assert_eq!(first.documents_for_mount(1).await.unwrap().len(), 1);
        drop(first);

        let second = Database::open(&temp.path().join("second.sqlite"))
            .await
            .unwrap();
        second
            .sync_mounts(std::slice::from_ref(&mount))
            .await
            .unwrap();
        let report = sync_mount(&second, 1, &root, &mut ids).await.unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(second.documents_for_mount(1).await.unwrap().len(), 1);
    }
}
