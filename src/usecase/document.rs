//! Reading and writing a page through the mount.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::{AppPaths, Config, Mount};
use crate::core::document::{content_hash, Document};
use crate::core::id::Snowflake;
use crate::db::Database;
use crate::index::indexer;
use crate::storage::filesystem::MountRoot;

#[derive(Debug, Clone)]
pub enum DocumentTarget {
    Id(i64),
    Path { mount_id: i64, path: String },
}

#[derive(Debug)]
pub struct ReadResult {
    pub mount: Mount,
    pub document: Option<Document>,
    pub relative_path: String,
    pub content: String,
    pub content_hash: String,
}

#[derive(Debug)]
pub struct WriteResult {
    pub mount: Mount,
    pub document: Document,
    pub content_hash: String,
}

pub async fn read(
    config: &Config,
    database: &Database,
    target: DocumentTarget,
) -> Result<ReadResult> {
    let (mount, relative_path, document) = resolve(config, database, target).await?;
    let root = MountRoot::new(&mount.directory_path)?;
    let content = root.read_markdown(Path::new(&relative_path))?;
    let hash = content_hash(&content);
    Ok(ReadResult {
        mount,
        document,
        relative_path,
        content,
        content_hash: hash,
    })
}

/// Writes a page and refreshes its index.
///
/// Updating an existing file requires `expected_hash` from a previous read.
/// Creating a new file requires `expected_hash` to be absent.
pub async fn write(
    paths: &AppPaths,
    database: &Database,
    target: DocumentTarget,
    content: &str,
    expected_hash: Option<&str>,
) -> Result<WriteResult> {
    let _lock = paths.lock()?;
    // Reload config under the lock so a concurrently added mount is visible.
    let config = Config::load_or_create(&paths.config_file())?;
    let (mount, relative_path, _) = resolve(&config, database, target).await?;
    let root = MountRoot::new(&mount.directory_path)?;
    let relative = Path::new(&relative_path);
    let hash = content_hash(content);

    if root.exists(relative)? {
        let expected =
            expected_hash.context("expected_hash is required to update an existing page")?;
        let current = root.read_markdown(relative)?;
        let current_hash = content_hash(&current);
        if current_hash != expected {
            bail!("page changed since it was read: expected {expected}, found {current_hash}");
        }
        root.write_atomic(relative, content)?;
    } else {
        if expected_hash.is_some() {
            bail!("page does not exist; omit expected_hash to create it");
        }
        root.create_new(relative, content)?;
    }

    let mut ids = Snowflake::persistent(config.node_id, &paths.id_state_file())?;
    indexer::sync_mount(database, mount.id, &root, &mut ids).await?;

    let document = database
        .document_by_path(mount.id, &relative_path)
        .await?
        .with_context(|| format!("page {relative_path} was written but not indexed"))?;
    Ok(WriteResult {
        mount,
        document,
        content_hash: hash,
    })
}

async fn resolve(
    config: &Config,
    database: &Database,
    target: DocumentTarget,
) -> Result<(Mount, String, Option<Document>)> {
    match target {
        DocumentTarget::Id(id) => {
            let document = database
                .get_document(id)
                .await?
                .with_context(|| format!("page {id} is not indexed"))?;
            let mount = config
                .find_by_id(document.mount_id)
                .with_context(|| format!("mount {} is not registered", document.mount_id))?
                .clone();
            Ok((mount, document.relative_path.clone(), Some(document)))
        }
        DocumentTarget::Path { mount_id, path } => {
            let mount = config
                .find_by_id(mount_id)
                .with_context(|| format!("mount {mount_id} is not registered"))?
                .clone();
            let relative_path = clean_path(&path)?;
            let document = database.document_by_path(mount_id, &relative_path).await?;
            Ok((mount, relative_path, document))
        }
    }
}

fn clean_path(path: &str) -> Result<String> {
    if path.contains('\0') {
        bail!("path must not contain NUL");
    }
    if path.starts_with('/') {
        bail!("absolute paths are not allowed");
    }

    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("parent directory components are not allowed"),
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        bail!("path must name a file");
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;

    struct Fixture {
        _temp: tempfile::TempDir,
        paths: AppPaths,
        config: Config,
        database: Database,
        mount: Mount,
    }

    async fn fixture() -> Fixture {
        let temp = tempdir().unwrap();
        let project = temp.path().join("project");
        let notes = project.join("notes");
        fs::create_dir_all(&notes).unwrap();

        let paths = AppPaths::new(temp.path().join("config"));
        paths.ensure_dirs().unwrap();
        let mount = Mount {
            id: 1,
            name: "notes".into(),
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

        Fixture {
            _temp: temp,
            paths,
            config,
            database,
            mount,
        }
    }

    #[tokio::test]
    async fn reads_by_id_and_path() {
        let fixture = fixture().await;
        fs::write(
            fixture.mount.directory_path.join("page.md"),
            "---\nid: 42\n---\n# Page\n",
        )
        .unwrap();
        let root = MountRoot::new(&fixture.mount.directory_path).unwrap();
        let mut ids = Snowflake::new(1).unwrap();
        indexer::sync_mount(&fixture.database, 1, &root, &mut ids)
            .await
            .unwrap();

        let by_id = read(&fixture.config, &fixture.database, DocumentTarget::Id(42))
            .await
            .unwrap();
        assert_eq!(by_id.relative_path, "page.md");
        assert!(by_id.content.contains("# Page"));

        let by_path = read(
            &fixture.config,
            &fixture.database,
            DocumentTarget::Path {
                mount_id: 1,
                path: "./page.md".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(by_path.content, by_id.content);
        assert_eq!(by_path.document.unwrap().id, 42);
    }

    #[tokio::test]
    async fn write_requires_the_expected_hash_and_creates_pages() {
        let fixture = fixture().await;
        fs::write(fixture.mount.directory_path.join("page.md"), "# Before\n").unwrap();
        let root = MountRoot::new(&fixture.mount.directory_path).unwrap();
        let mut ids = Snowflake::new(1).unwrap();
        indexer::sync_mount(&fixture.database, 1, &root, &mut ids)
            .await
            .unwrap();
        let before = read(
            &fixture.config,
            &fixture.database,
            DocumentTarget::Path {
                mount_id: 1,
                path: "page.md".into(),
            },
        )
        .await
        .unwrap();

        let error = write(
            &fixture.paths,
            &fixture.database,
            DocumentTarget::Path {
                mount_id: 1,
                path: "page.md".into(),
            },
            "# After\n",
            Some("wrong"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("changed since it was read"));
        assert_eq!(
            fs::read_to_string(fixture.mount.directory_path.join("page.md")).unwrap(),
            "# Before\n"
        );

        let written = write(
            &fixture.paths,
            &fixture.database,
            DocumentTarget::Path {
                mount_id: 1,
                path: "page.md".into(),
            },
            "# After\n",
            Some(&before.content_hash),
        )
        .await
        .unwrap();
        assert_eq!(written.document.relative_path, "page.md");

        let created = write(
            &fixture.paths,
            &fixture.database,
            DocumentTarget::Path {
                mount_id: 1,
                path: "new.md".into(),
            },
            "# New\n",
            None,
        )
        .await
        .unwrap();
        assert_eq!(created.document.relative_path, "new.md");
        assert!(fixture.mount.directory_path.join("new.md").is_file());
    }
}
