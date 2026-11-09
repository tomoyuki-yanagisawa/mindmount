//! Extract and resolve links without reading outside the index.

use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Event, Parser, Tag};

use crate::core::document::{parse_document, Document};

#[derive(Debug, PartialEq, Eq)]
pub struct Reference {
    pub target: String,
    pub anchor: Option<String>,
    pub raw: String,
    pub kind: &'static str,
}

pub fn extract(content: &str) -> Vec<Reference> {
    let body = parse_document(content).body;
    let mut result = Vec::new();
    let mut text_start = None;
    let mut text_end = 0;
    let mut link_depth = 0;
    let mut code_block = false;
    for (event, range) in Parser::new(body).into_offset_iter() {
        let text = matches!(event, Event::Text(_)) && link_depth == 0 && !code_block;
        if !text || text_start.is_some() && range.start != text_end {
            if let Some(start) = text_start.take() {
                extract_wiki(&body[start..text_end], &mut result);
            }
        }
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_block = true,
            Event::End(pulldown_cmark::TagEnd::CodeBlock) => code_block = false,
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_depth += 1;
                let (target, anchor) = split_anchor(&dest_url);
                result.push(Reference {
                    target,
                    anchor,
                    raw: body[range].to_owned(),
                    kind: "markdown",
                });
            }
            Event::End(pulldown_cmark::TagEnd::Link) => link_depth -= 1,
            Event::Text(_) if link_depth == 0 && !code_block => {
                text_start.get_or_insert(range.start);
                text_end = range.end;
            }
            _ => {}
        }
    }
    if let Some(start) = text_start {
        extract_wiki(&body[start..text_end], &mut result);
    }
    result
}

fn extract_wiki(text: &str, result: &mut Vec<Reference>) {
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let Some(end) = rest[start + 2..].find("]]") else {
            break;
        };
        let raw = &rest[start..start + end + 4];
        let value = raw[2..raw.len() - 2].split('|').next().unwrap_or("").trim();
        if !value.is_empty() {
            let (target, anchor) = split_anchor(value);
            result.push(Reference {
                target,
                anchor,
                raw: raw.to_owned(),
                kind: "wiki",
            });
        }
        rest = &rest[start + end + 4..];
    }
}

fn split_anchor(value: &str) -> (String, Option<String>) {
    match value.split_once('#') {
        Some((target, anchor)) => (target.to_owned(), Some(anchor.to_owned())),
        None => (value.to_owned(), None),
    }
}

pub fn resolve(reference: &Reference, source: &Document, documents: &[Document]) -> Option<i64> {
    let target = reference.target.as_str();
    if target.contains("://") || target.starts_with("mailto:") || target.starts_with('/') {
        return None;
    }
    if target.is_empty() {
        return reference.anchor.as_ref().map(|_| source.id);
    }
    let relative = Path::new(&source.relative_path)
        .parent()
        .unwrap_or(Path::new(""));
    let explicit =
        reference.kind == "markdown" || target.starts_with("./") || target.starts_with("../");
    let path = normalized(if explicit {
        relative.join(target)
    } else {
        PathBuf::from(target)
    })?;
    let candidates: Vec<_> = documents
        .iter()
        .filter(|doc| {
            if doc.mount_id != source.mount_id {
                return false;
            }
            let p = Path::new(&doc.relative_path);
            if explicit {
                p == path
            } else {
                p == path
                    || p.with_extension("") == path
                    || (!target.contains('/')
                        && (p.file_stem().and_then(|s| s.to_str()) == Some(target)
                            || doc.title == target))
            }
        })
        .collect();
    (candidates.len() == 1).then(|| candidates[0].id)
}

fn normalized(path: PathBuf) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return None,
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_links_but_not_code_or_frontmatter() {
        let refs = extract("---\ntitle: '[[Hidden]]'\n---\n[Page](./page.md#part) [[Note|label]] `[[code]]`\n\n```\n[[fenced]]\n```\n");
        assert_eq!(refs.len(), 2);
        assert_eq!(
            (&refs[0].target, refs[0].anchor.as_deref()),
            (&"./page.md".to_owned(), Some("part"))
        );
        assert_eq!(
            (&refs[1].target, refs[1].kind),
            (&"Note".to_owned(), "wiki")
        );
    }
}
