//! Markdown document metadata extraction.
//!
//! Markdown files are the source of truth. This module only reads them: it
//! extracts an optional `id` and `title` from frontmatter, derives a display
//! title, and hashes the content for change detection.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

/// A document row as stored in the derived index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub id: i64,
    pub mount_id: i64,
    pub relative_path: String,
    pub title: String,
    pub content_hash: String,
    pub modified_at: DateTime<Utc>,
    pub indexed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub id: Option<i64>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDocument<'a> {
    pub frontmatter: Frontmatter,
    pub body: &'a str,
}

impl ParsedDocument<'_> {
    /// Frontmatter title, then the first ATX H1, then `fallback`.
    pub fn title(&self, fallback: &str) -> String {
        if let Some(title) = &self.frontmatter.title {
            let title = title.trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }

        first_h1(self.body).unwrap_or_else(|| fallback.to_string())
    }
}

pub fn parse_document(content: &str) -> ParsedDocument<'_> {
    let (frontmatter, body) = split_frontmatter(content);
    ParsedDocument {
        frontmatter: frontmatter.map(parse_frontmatter).unwrap_or_default(),
        body,
    }
}

pub fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hash, "{byte:02x}");
    }
    hash
}

fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let Some(first_line_end) = content.find('\n') else {
        return (None, content);
    };
    if content[..first_line_end].trim_end_matches('\r') != "---" {
        return (None, content);
    }

    let frontmatter_start = first_line_end + 1;
    let mut offset = frontmatter_start;
    while offset <= content.len() {
        let line_end = content[offset..]
            .find('\n')
            .map_or(content.len(), |index| offset + index);
        let line = content[offset..line_end].trim_end_matches('\r');
        if line == "---" || line == "..." {
            let frontmatter = &content[frontmatter_start..offset];
            let body_start = if line_end < content.len() {
                line_end + 1
            } else {
                line_end
            };
            return (Some(frontmatter), &content[body_start..]);
        }
        if line_end == content.len() {
            break;
        }
        offset = line_end + 1;
    }

    // An opening `---` without a closing delimiter is not frontmatter.
    (None, content)
}

fn parse_frontmatter(text: &str) -> Frontmatter {
    let mut frontmatter = Frontmatter::default();

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty()
            || line.starts_with(' ')
            || line.starts_with('\t')
            || line.trim_start().starts_with('#')
        {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = parse_scalar(value);

        match key.trim() {
            "id" => {
                if let Ok(id) = value.parse::<i64>() {
                    if id > 0 {
                        frontmatter.id = Some(id);
                    }
                }
            }
            "title" if !value.is_empty() => {
                frontmatter.title = Some(value);
            }
            _ => {}
        }
    }

    frontmatter
}

fn parse_scalar(value: &str) -> String {
    let value = value.trim();

    let (quoted, value) = if let Some(rest) = value.strip_prefix('"') {
        (true, rest.strip_suffix('"').unwrap_or(rest))
    } else if let Some(rest) = value.strip_prefix('\'') {
        (true, rest.strip_suffix('\'').unwrap_or(rest))
    } else {
        (false, value)
    };

    if !quoted {
        if let Some(index) = value.find(" #") {
            return value[..index].trim().to_string();
        }
    }

    value.trim().to_string()
}

fn first_h1(body: &str) -> Option<String> {
    let mut fence: Option<&str> = None;

    for raw_line in body.lines() {
        let line = raw_line.trim_start();

        if let Some(marker) = fence {
            if line.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if line.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }

        let bytes = line.as_bytes();
        if bytes.first() != Some(&b'#') {
            continue;
        }
        let hashes = bytes.iter().take_while(|byte| **byte == b'#').count();
        if hashes != 1 {
            continue;
        }
        if bytes.get(1).is_some_and(|byte| !byte.is_ascii_whitespace()) {
            continue;
        }

        let title = line[1..].trim().trim_end_matches('#').trim();
        if !title.is_empty() {
            return Some(title.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_id_and_title_from_frontmatter() {
        let document = parse_document(
            "---\nid: 1234567890123456789\ntitle: \"Architecture\"\n---\n\n# Ignored heading\n",
        );

        assert_eq!(document.frontmatter.id, Some(1234567890123456789));
        assert_eq!(document.frontmatter.title.as_deref(), Some("Architecture"));
        assert_eq!(document.title("fallback"), "Architecture");
        assert!(document.body.starts_with("\n# Ignored heading"));
    }

    #[test]
    fn reads_plain_scalars_and_strips_comments() {
        let document = parse_document("---\nid: 42 # stable\ntitle: 'Storage'\n---\nbody\n");

        assert_eq!(document.frontmatter.id, Some(42));
        assert_eq!(document.title("fallback"), "Storage");
    }

    #[test]
    fn falls_back_to_the_first_h1_and_skips_code_fences() {
        let document = parse_document("```sh\n# not a title\n```\n\n## Section\n\n# Real title\n");

        assert_eq!(document.frontmatter, Frontmatter::default());
        assert_eq!(document.title("fallback"), "Real title");
    }

    #[test]
    fn strips_trailing_heading_hashes() {
        let document = parse_document("# Title ###\n");
        assert_eq!(document.title("fallback"), "Title");
    }

    #[test]
    fn falls_back_when_there_is_no_title() {
        let document = parse_document("plain text\n");
        assert_eq!(document.title("file-name"), "file-name");
    }

    #[test]
    fn ignores_invalid_frontmatter_ids() {
        let document = parse_document("---\nid: nope\ntitle: \"\"\n---\n");
        assert_eq!(document.frontmatter.id, None);
        assert_eq!(document.frontmatter.title, None);
    }

    #[test]
    fn requires_a_closing_frontmatter_delimiter() {
        let document = parse_document("---\nid: 1\nplain\n");
        assert_eq!(document.frontmatter, Frontmatter::default());
        assert_eq!(document.title("fallback"), "fallback");
    }

    #[test]
    fn hashes_content_with_sha256() {
        assert_eq!(
            content_hash("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(content_hash("hello"), content_hash("hello"));
        assert_ne!(content_hash("hello"), content_hash("hello\n"));
    }
}
