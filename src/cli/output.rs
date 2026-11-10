//! CLI rendering for operation results.

use anyhow::{bail, Result};

use crate::config::{Config, Mount};
use crate::core::document::Document;
use crate::db::queries::{LinkRow, MetadataEntry};
use crate::index::indexer::SyncOutcome;
use crate::search::SearchHit;
use crate::usecase::mount::{MountResult, MountRow};

pub fn mount_result(result: &MountResult) {
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

pub fn mounts(rows: &[MountRow]) {
    if rows.is_empty() {
        println!("No mounts registered.");
        return;
    }

    println!("ID\tNAME\tDOCUMENTS\tDIRECTORY");
    for row in rows {
        println!(
            "{}\t{}\t{}\t{}",
            row.mount.id,
            row.mount.name,
            row.documents,
            row.mount.directory_path.display()
        );
    }
}

pub fn unmounted(mount: &Mount) {
    println!("Unmounted \"{}\" ({})", mount.name, mount.id);
    println!("Markdown files were left untouched.");
    println!("Index metadata and knowledge links were kept inactive.");
}

pub fn page_list(config: &Config, documents: &[Document], empty: &str) {
    if documents.is_empty() {
        println!("{empty}");
        return;
    }

    println!("ID\tMOUNT\tPATH\tTITLE");
    for document in documents {
        let name = config
            .find_by_id(document.mount_id)
            .map(|mount| mount.name.as_str())
            .unwrap_or("(unknown)");
        println!(
            "{}\t{}\t{}\t{}",
            document.id, name, document.relative_path, document.title
        );
    }
}

pub fn search_hits(hits: &[SearchHit]) {
    if hits.is_empty() {
        println!("No matching pages.");
        return;
    }

    println!("MOUNT\tPATH\tLINE\tTEXT");
    for hit in hits {
        println!(
            "{}\t{}\t{}\t{}",
            hit.mount_name, hit.relative_path, hit.line, hit.text
        );
    }
}

pub fn metadata(entries: &[MetadataEntry]) {
    if entries.is_empty() {
        println!("No metadata.");
        return;
    }

    println!("KEY\tVALUE\tUPDATED_BY\tUPDATED_AT");
    for entry in entries {
        println!(
            "{}\t{}\t{}\t{}",
            entry.key,
            entry.value_json,
            entry.updated_by,
            entry.updated_at.to_rfc3339()
        );
    }
}

pub fn links(links: &[LinkRow]) {
    if links.is_empty() {
        println!("No links found.");
        return;
    }

    println!("SOURCE_ID\tSOURCE_PATH\tTARGET_ID\tTARGET\tKIND");
    for link in links {
        let target = match &link.anchor {
            Some(anchor) => format!("{}#{anchor}", link.target),
            None => link.target.clone(),
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
}

pub fn reindex(outcome: &SyncOutcome) -> Result<()> {
    if outcome.synced.is_empty() && outcome.failures.is_empty() {
        println!("No mounts registered.");
        return Ok(());
    }
    for synced in &outcome.synced {
        println!(
            "Reindexed \"{}\" ({}): {}",
            synced.mount.name,
            synced.mount.id,
            synced.report.summary()
        );
    }
    for failure in &outcome.failures {
        eprintln!(
            "error: cannot reindex \"{}\" ({}): {}",
            failure.mount.name, failure.mount.id, failure.message
        );
    }
    if !outcome.failures.is_empty() {
        bail!("reindex completed with {} error(s)", outcome.failures.len());
    }
    Ok(())
}
