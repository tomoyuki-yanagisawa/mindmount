//! Literal grep over the mounted Markdown files.
//!
//! Search reads the files themselves, so the index never makes results stale.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use tracing::warn;

use crate::config::Config;

pub struct SearchOptions<'a> {
    pub query: &'a str,
    pub mount_id: Option<i64>,
    pub limit: usize,
}

#[derive(Debug)]
pub struct SearchHit {
    pub document_id: Option<i64>,
    pub mount_id: i64,
    pub mount_name: String,
    pub relative_path: String,
    pub line: usize,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

const CONTEXT_LINES: usize = 2;

pub fn search(config: &Config, options: SearchOptions<'_>) -> Result<Vec<SearchHit>> {
    let matcher = RegexBuilder::new(&regex::escape(options.query))
        .case_insensitive(true)
        .build()
        .context("cannot build the search matcher")?;
    let mut hits = Vec::new();

    for mount in &config.mounts {
        if options.mount_id.is_some_and(|id| id != mount.id) {
            continue;
        }
        collect_mount(mount, &matcher, options.limit, &mut hits)?;
        if hits.len() >= options.limit {
            break;
        }
    }

    Ok(hits)
}

fn collect_mount(
    mount: &crate::config::Mount,
    matcher: &Regex,
    limit: usize,
    hits: &mut Vec<SearchHit>,
) -> Result<()> {
    if !mount.directory_path.is_dir() {
        return Ok(());
    }

    let mut walker = WalkBuilder::new(&mount.directory_path);
    walker
        .hidden(true)
        .git_ignore(true)
        .follow_links(false)
        .require_git(false);

    for entry in walker.build() {
        if hits.len() >= limit {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn!(error = %error, "skipping an unreadable search entry");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) || !is_markdown(entry.path()) {
            continue;
        }
        let content = match fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(error) => {
                warn!(path = %entry.path().display(), error = %error, "skipping an unreadable file");
                continue;
            }
        };
        let relative = match relative_path(entry.path(), &mount.directory_path) {
            Some(relative) => relative,
            None => continue,
        };
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !matcher.is_match(line) {
                continue;
            }
            let start = index.saturating_sub(CONTEXT_LINES);
            let end = (index + CONTEXT_LINES + 1).min(lines.len());
            hits.push(SearchHit {
                document_id: None,
                mount_id: mount.id,
                mount_name: mount.name.clone(),
                relative_path: relative.clone(),
                line: index + 1,
                text: (*line).to_string(),
                before: lines[start..index]
                    .iter()
                    .map(|line| (*line).into())
                    .collect(),
                after: lines[index + 1..end]
                    .iter()
                    .map(|line| (*line).into())
                    .collect(),
            });
            if hits.len() >= limit {
                break;
            }
        }
    }

    Ok(())
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

fn relative_path(path: &Path, root: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let parts: Vec<&str> = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}
