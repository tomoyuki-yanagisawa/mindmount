//! Builds the derived document index for a mount.
//!
//! Markdown files are authoritative. This module scans them, reads their
//! metadata and links, and applies the resulting changes to SQLite in one
//! transaction. The database only stores what this module computes.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use tracing::warn;

use crate::config::{Config, Mount};
use crate::core::document::{content_hash, parse_document, Document};
use crate::core::id::Snowflake;
use crate::core::reference::{self, ResolvedReference};
use crate::db::Database;
use crate::storage::filesystem::MountRoot;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub missing: usize,
    pub unchanged: usize,
    pub errors: usize,
}

impl SyncReport {
    pub fn summary(&self) -> String {
        format!(
            "{} files, {} added, {} updated, {} missing, {} unchanged, {} errors",
            self.scanned, self.added, self.updated, self.missing, self.unchanged, self.errors
        )
    }
}

#[derive(Debug)]
pub struct MountSync {
    pub mount: Mount,
    pub report: SyncReport,
}

#[derive(Debug)]
pub struct SyncFailure {
    pub mount: Mount,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub synced: Vec<MountSync>,
    pub failures: Vec<SyncFailure>,
}

/// Scans every registered mount and updates the derived index.
///
/// A mount that fails is reported in the outcome and does not stop the rest.
pub async fn sync_all(
    config: &Config,
    database: &Database,
    ids: &mut Snowflake,
) -> Result<SyncOutcome> {
    database.sync_mounts(&config.mounts).await?;

    let mut outcome = SyncOutcome::default();
    for mount in &config.mounts {
        let result = match MountRoot::new(&mount.directory_path) {
            Ok(root) => sync_mount(database, mount.id, &root, ids).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(report) => outcome.synced.push(MountSync {
                mount: mount.clone(),
                report,
            }),
            Err(error) => outcome.failures.push(SyncFailure {
                mount: mount.clone(),
                message: format!("{error:#}"),
            }),
        }
    }
    Ok(outcome)
}

pub async fn sync_mount(
    database: &Database,
    mount_id: i64,
    root: &MountRoot,
    ids: &mut Snowflake,
) -> Result<SyncReport> {
    let existing: HashMap<String, Document> = database
        .documents_for_mount(mount_id)
        .await?
        .into_iter()
        .map(|document| (document.relative_path.clone(), document))
        .collect();
    let previous_documents: Vec<Document> = existing.values().cloned().collect();

    let files = root.scan_markdown()?;
    let mut report = SyncReport {
        scanned: files.len(),
        ..SyncReport::default()
    };
    let mut scanned_paths = HashSet::new();
    let mut documents = Vec::with_capacity(files.len());
    let mut bodies = HashMap::new();
    let mut upserts = Vec::new();

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
                // Keep its metadata so other pages still resolve links to it.
                if let Some(existing_document) = existing.get(&relative_path) {
                    documents.push(existing_document.clone());
                }
                continue;
            }
        };

        let hash = content_hash(&content);
        if let Some(existing_document) = existing.get(&relative_path) {
            if existing_document.content_hash == hash {
                report.unchanged += 1;
                bodies.insert(existing_document.id, content);
                documents.push(existing_document.clone());
                continue;
            }
        }

        let parsed = parse_document(&content);
        let title = parsed.title(&fallback_title(relative));
        let id = match parsed.frontmatter.id {
            Some(id) => id,
            None => match existing.get(&relative_path) {
                Some(existing_document) => existing_document.id,
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

        let document = Document {
            id,
            mount_id,
            relative_path,
            title,
            content_hash: hash,
            modified_at,
            indexed_at: Utc::now(),
        };
        bodies.insert(id, content);
        documents.push(document.clone());
        upserts.push(document);
    }

    let already_missing = database.missing_paths(mount_id).await?;
    let missing: Vec<String> = existing
        .keys()
        .filter(|path| !scanned_paths.contains(*path) && !already_missing.contains(*path))
        .cloned()
        .collect();
    report.missing = missing.len();

    // Catch duplicate frontmatter IDs before touching the database so the
    // whole mount stays consistent.
    let mut owners: HashMap<i64, String> = HashMap::new();
    for document in &documents {
        if let Some(other_path) = owners.insert(document.id, document.relative_path.clone()) {
            bail!(
                "document id {} is used by both {} and {}",
                document.id,
                other_path,
                document.relative_path
            );
        }
    }

    // Nothing changed, so the stored references are already current.
    let (references, affected) = if upserts.is_empty() && missing.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let changed: HashSet<i64> = upserts.iter().map(|document| document.id).collect();
        resolved_changes(&documents, &previous_documents, &bodies, &changed)
    };
    database
        .apply_mount_sync(mount_id, &upserts, &missing, &references, &affected)
        .await?;

    Ok(report)
}

/// Resolves the current links and returns the rows to persist.
///
/// A page is rewritten when its own content changed, when a link target was
/// replaced or removed, or when the new document set changes how its links
/// resolve. Pages whose links still resolve the same way are left untouched,
/// so a normal edit only touches the file itself and its backlinks.
fn resolved_changes(
    documents: &[Document],
    previous: &[Document],
    bodies: &HashMap<i64, String>,
    changed: &HashSet<i64>,
) -> (Vec<ResolvedReference>, Vec<i64>) {
    let mut references = Vec::new();
    let mut affected = Vec::new();

    for document in documents {
        let Some(body) = bodies.get(&document.id) else {
            continue;
        };
        let extracted = reference::extract(body);
        let new_targets: Vec<Option<i64>> = extracted
            .iter()
            .map(|reference| reference::resolve(reference, document, documents))
            .collect();

        let is_affected = if changed.contains(&document.id) {
            true
        } else {
            let old_targets: Vec<Option<i64>> = extracted
                .iter()
                .map(|reference| reference::resolve(reference, document, previous))
                .collect();
            old_targets != new_targets
                || new_targets.iter().flatten().any(|id| changed.contains(id))
        };
        if !is_affected {
            continue;
        }

        affected.push(document.id);
        references.extend(
            extracted
                .into_iter()
                .zip(new_targets)
                .map(|(reference, target_id)| ResolvedReference {
                    source_id: document.id,
                    target_id,
                    target: reference.target,
                    anchor: reference.anchor,
                    raw: reference.raw,
                    kind: reference.kind,
                    line: reference.line,
                }),
        );
    }

    (references, affected)
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
    async fn adds_updates_and_marks_missing_documents_while_preserving_ids() {
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
        assert_eq!(report.missing, 1);
        assert_eq!(report.unchanged, 0);

        let after = fixture.database.documents_for_mount(1).await.unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(
            fixture
                .database
                .list_documents(Some(1))
                .await
                .unwrap()
                .len(),
            2
        );
        let index = after
            .iter()
            .find(|document| document.relative_path == "index.md")
            .unwrap();
        assert_eq!(index.id, index_id);
        assert_eq!(index.title, "Updated");

        let report = fixture.sync().await.unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.unchanged, 2);
        assert_eq!(report.added + report.updated + report.missing, 0);
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
            .apply_mount_sync(1, &[document], &[], &[], &[id])
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
        assert_eq!(documents.len(), 2);
        let moved = documents.iter().find(|document| document.id == 42).unwrap();
        assert_eq!(moved.relative_path, "new.md");
        let visible = fixture.database.list_documents(Some(1)).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(
            (visible[0].id, visible[0].relative_path.as_str()),
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

    fn document(id: i64, path: &str, title: &str) -> Document {
        Document {
            id,
            mount_id: 1,
            relative_path: path.into(),
            title: title.into(),
            content_hash: String::new(),
            modified_at: Utc::now(),
            indexed_at: Utc::now(),
        }
    }

    #[test]
    fn editing_a_target_updates_its_backlinks_only() {
        let documents = vec![
            document(10, "source.md", "Source"),
            document(20, "target.md", "Target"),
            document(30, "other.md", "Other"),
        ];
        let bodies = HashMap::from([
            (10, "[Target](target.md)\n".to_string()),
            (20, "# Target\n\nedited\n".to_string()),
            (30, "no links\n".to_string()),
        ]);
        let changed = HashSet::from([20]);

        let (references, affected) = resolved_changes(&documents, &documents, &bodies, &changed);

        assert_eq!(affected, vec![10, 20]);
        assert!(references
            .iter()
            .any(|reference| reference.source_id == 10 && reference.target_id == Some(20)));
    }

    #[test]
    fn adding_a_page_re_resolves_existing_links() {
        let source = document(10, "source.md", "Source");
        let first = document(20, "first.md", "Shared");
        let second = document(21, "second.md", "Shared");
        let previous = vec![source.clone(), first.clone()];
        let documents = vec![source, first, second];
        let bodies = HashMap::from([
            (10, "[[Shared]]\n".to_string()),
            (20, String::new()),
            (21, String::new()),
        ]);
        let changed = HashSet::from([21]);

        let (references, affected) = resolved_changes(&documents, &previous, &bodies, &changed);

        assert!(affected.contains(&10));
        assert!(affected.contains(&21));
        assert!(references
            .iter()
            .any(|reference| reference.source_id == 10 && reference.target_id.is_none()));
    }
}
