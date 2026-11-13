//! Safe access to files inside a mounted directory.
//!
//! Every path is resolved relative to a canonicalized mount root. Absolute
//! paths, parent-directory components, and symlinks that escape the root are
//! rejected. Markdown files are never created or modified by scanning.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct MountRoot {
    root: PathBuf,
}

impl MountRoot {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let root = path
            .canonicalize()
            .with_context(|| format!("cannot open mount directory {}", path.display()))?;
        if !root.is_dir() {
            bail!("mount path is not a directory: {}", path.display());
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads a Markdown file without modifying it.
    pub fn read_markdown(&self, relative: &Path) -> Result<String> {
        let path = self.resolve_existing(relative)?;
        if !path.is_file() {
            bail!("not a file: {}", relative.display());
        }
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", relative.display()))
    }

    pub fn modified_at(&self, relative: &Path) -> Result<DateTime<Utc>> {
        let path = self.resolve_existing(relative)?;
        let modified = fs::metadata(&path)
            .with_context(|| format!("cannot read metadata for {}", relative.display()))?
            .modified()
            .with_context(|| format!("cannot read mtime for {}", relative.display()))?;
        Ok(DateTime::<Utc>::from(modified))
    }

    /// Returns every visible Markdown file, relative to the mount root.
    ///
    /// Hidden files and directories, non-Markdown files, and symlinks are
    /// skipped. The result is sorted for deterministic indexing.
    pub fn scan_markdown(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let entries = WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden(entry.file_name()));
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot scan {}", self.root.display()))?;
            if entry.file_type().is_file() && is_markdown(entry.file_name()) {
                files.push(entry.path().strip_prefix(&self.root)?.to_path_buf());
            }
        }
        files.sort();
        Ok(files)
    }

    fn resolve_existing(&self, relative: &Path) -> Result<PathBuf> {
        let joined = self.join_checked(relative)?;
        let canonical = joined
            .canonicalize()
            .with_context(|| format!("cannot resolve {}", relative.display()))?;
        if !canonical.starts_with(&self.root) {
            bail!("path escapes the mount root: {}", relative.display());
        }
        Ok(canonical)
    }

    fn join_checked(&self, relative: &Path) -> Result<PathBuf> {
        validate_relative(relative)?;
        Ok(self.root.join(relative))
    }
}

fn validate_relative(relative: &Path) -> Result<()> {
    if relative.as_os_str().is_empty() {
        bail!("empty path");
    }
    if relative.is_absolute() {
        bail!("absolute paths are not allowed: {}", relative.display());
    }

    let mut has_normal_component = false;
    for component in relative.components() {
        match component {
            Component::Normal(_) => has_normal_component = true,
            Component::CurDir => {}
            Component::ParentDir => {
                bail!(
                    "parent directory components are not allowed: {}",
                    relative.display()
                )
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute paths are not allowed: {}", relative.display())
            }
        }
    }

    if !has_normal_component {
        bail!("path must name a file: {}", relative.display());
    }
    Ok(())
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

fn is_markdown(name: &std::ffi::OsStr) -> bool {
    let Some(extension) = Path::new(name).extension() else {
        return false;
    };
    let extension = extension.to_string_lossy();
    extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn relative_paths(root: &Path) -> Vec<String> {
        MountRoot::new(root)
            .unwrap()
            .scan_markdown()
            .unwrap()
            .into_iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn scans_only_visible_markdown_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("notes");
        write(&root.join("index.md"), "# Index\n");
        write(&root.join("decisions/storage.md"), "# Storage\n");
        write(&root.join("README.markdown"), "# Readme\n");
        write(&root.join("notes.txt"), "not markdown\n");
        write(&root.join(".hidden.md"), "hidden\n");
        write(&root.join(".git/config.md"), "hidden directory\n");

        assert_eq!(
            relative_paths(&root),
            vec![
                "README.markdown".to_string(),
                "decisions/storage.md".to_string(),
                "index.md".to_string()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_skips_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("notes");
        let outside = temp.path().join("outside");
        write(&root.join("index.md"), "# Index\n");
        write(&outside.join("secret.md"), "# Secret\n");
        symlink(outside.join("secret.md"), root.join("linked.md")).unwrap();
        symlink(&outside, root.join("linked-dir")).unwrap();

        assert_eq!(relative_paths(&root), vec!["index.md".to_string()]);
    }

    #[test]
    fn rejects_path_traversal_and_absolute_paths() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("notes");
        write(&root.join("index.md"), "# Index\n");
        write(&temp.path().join("secret.md"), "# Secret\n");
        let mount = MountRoot::new(&root).unwrap();

        assert!(mount.read_markdown(Path::new("../secret.md")).is_err());
        assert!(mount.read_markdown(Path::new("/etc/passwd")).is_err());
        assert!(mount.read_markdown(Path::new("")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escapes_on_read() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("notes");
        let outside = temp.path().join("outside");
        write(&root.join("index.md"), "# Index\n");
        write(&outside.join("secret.md"), "# Secret\n");
        symlink(outside.join("secret.md"), root.join("leak.md")).unwrap();
        let mount = MountRoot::new(&root).unwrap();

        let error = mount
            .read_markdown(Path::new("leak.md"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("escapes"), "{error}");
        assert_eq!(
            fs::read_to_string(outside.join("secret.md")).unwrap(),
            "# Secret\n"
        );
    }

    #[test]
    fn scanning_does_not_modify_files() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("notes");
        let page = root.join("index.md");
        write(&page, "# Index\n");
        let before = fs::read_to_string(&page).unwrap();
        let before_modified = fs::metadata(&page).unwrap().modified().unwrap();

        let _ = MountRoot::new(&root).unwrap().scan_markdown().unwrap();

        assert_eq!(fs::read_to_string(&page).unwrap(), before);
        assert_eq!(
            fs::metadata(&page).unwrap().modified().unwrap(),
            before_modified
        );
    }
}
