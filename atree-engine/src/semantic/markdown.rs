//! Markdown section extraction — heading structure and cross-links.
//!
//! Modeled after GitNexusRelay's:
//! - `gitnexus/src/core/ingestion/pipeline-phases/markdown.ts`
//! - `gitnexus/src/core/markdown-processor.ts`
//!
//! Line-by-line scanner (no regex). Extracts heading structure from .md/.mdx
//! files, creating Section nodes with CONTAINS edges and cross-link edges.

use serde::{Serialize, Deserialize};

// ── Markdown section types ───────────────────────────────────────────────────

/// A markdown heading found in a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownHeading {
    /// Heading level (1-6).
    pub level: u8,
    /// Heading text.
    pub text: String,
    /// 0-based line number.
    pub line: usize,
    /// Slug/anchor for this heading (e.g., "getting-started").
    pub slug: String,
}

/// A cross-link between markdown files or sections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownLink {
    /// Source file path.
    pub source_file: String,
    /// Target file path (relative).
    pub target_file: String,
    /// Optional section anchor (e.g., "#getting-started").
    pub target_anchor: Option<String>,
    /// 0-based line number of the link.
    pub line: usize,
}

/// Result of markdown processing.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarkdownResult {
    pub sections: usize,
    pub links: usize,
    pub headings: Vec<MarkdownHeading>,
    pub cross_links: Vec<MarkdownLink>,
}

// ── Heading parsing ──────────────────────────────────────────────────────────

/// Parse a heading from a line. Returns (level, text) if it's a heading.
fn parse_heading(line: &str) -> Option<(u8, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with('#') {
        return None;
    }

    // Count leading #s (max 6)
    let mut level = 0usize;
    for c in trimmed.as_bytes() {
        if *c == b'#' && level < 6 {
            level += 1;
        } else {
            break;
        }
    }

    if level == 0 || level > 6 {
        return None;
    }

    // Must have whitespace after the #s
    let rest = &trimmed[level..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }

    let text = rest.trim();
    if text.is_empty() {
        return None;
    }

    // Strip trailing #s (closing sequence)
    let text = text.trim_end_matches(['#', ' ', '\t']);

    Some((level as u8, text))
}

// ── Link parsing ─────────────────────────────────────────────────────────────

/// Parse all markdown links from a line.
fn parse_links(line: &str, line_idx: usize, file_path: &str, out: &mut Vec<MarkdownLink>) {
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Find '['
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }

        // Find matching ']'
        let Some(bracket_close) = bytes[i + 1..].iter().position(|&b| b == b']') else {
            i += 1;
            continue;
        };
        let bracket_close = i + 1 + bracket_close;

        // Next char must be '('
        if bracket_close + 1 >= bytes.len() || bytes[bracket_close + 1] != b'(' {
            i = bracket_close + 1;
            continue;
        }

        // Find matching ')'
        let Some(paren_close) = bytes[bracket_close + 2..].iter().position(|&b| b == b')') else {
            i = bracket_close + 2;
            continue;
        };
        let paren_close = bracket_close + 2 + paren_close;

        // Extract target
        let target = &line[bracket_close + 2..paren_close];

        // Skip external links
        if target.starts_with("http://") || target.starts_with("https://") {
            i = paren_close + 1;
            continue;
        }

        // Split into file and anchor
        if let Some(pos) = target.find('#') {
            let (file, anchor) = target.split_at(pos);
            out.push(MarkdownLink {
                source_file: file_path.to_string(),
                target_file: file.to_string(),
                target_anchor: Some(anchor[1..].to_string()),
                line: line_idx,
            });
        } else {
            out.push(MarkdownLink {
                source_file: file_path.to_string(),
                target_file: target.to_string(),
                target_anchor: None,
                line: line_idx,
            });
        }

        i = paren_close + 1;
    }
}

// ── Extraction ───────────────────────────────────────────────────────────────

/// Extract headings and cross-links from a markdown file.
pub fn extract_markdown(file_path: &str, content: &str) -> (Vec<MarkdownHeading>, Vec<MarkdownLink>) {
    let mut headings = Vec::new();
    let mut links = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        // Extract headings
        if let Some((level, text)) = parse_heading(line) {
            let slug = slugify(text);
            headings.push(MarkdownHeading {
                level,
                text: text.to_string(),
                line: line_idx,
                slug,
            });
        }

        // Extract links
        parse_links(line, line_idx, file_path, &mut links);
    }

    (headings, links)
}

/// Process multiple markdown files and return aggregated results.
pub fn process_markdown_files(files: &[(String, String)]) -> MarkdownResult {
    let mut result = MarkdownResult::default();
    for (path, content) in files {
        let (headings, links) = extract_markdown(path, content);
        result.sections += headings.len();
        result.links += links.len();
        result.headings.extend(headings);
        result.cross_links.extend(links);
    }
    result
}

/// Convert heading text to a URL slug.
fn slugify(text: &str) -> String {
    text.to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_headings() {
        let content = "# Getting Started\n\nSome text here.\n\n## Installation\n\nRun this command.\n\n### Advanced Configuration\n\nDetails here.\n";
        let (headings, _) = extract_markdown("README.md", content);
        assert_eq!(headings.len(), 3);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "Getting Started");
        assert_eq!(headings[0].slug, "getting-started");
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[1].text, "Installation");
        assert_eq!(headings[2].level, 3);
    }

    #[test]
    fn test_extract_cross_links() {
        let content = "# Guide\n\nSee [installation](INSTALL.md) for setup.\n\nCheck [config](docs/config.md#advanced) for details.\n\nVisit [external](https://example.com) for more.\n";
        let (_, links) = extract_markdown("README.md", content);
        assert_eq!(links.len(), 2); // external link skipped
        assert_eq!(links[0].target_file, "INSTALL.md");
        assert_eq!(links[0].target_anchor, None);
        assert_eq!(links[1].target_file, "docs/config.md");
        assert_eq!(links[1].target_anchor, Some("advanced".to_string()));
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Getting Started"), "getting-started");
        assert_eq!(slugify("API Reference (v2)"), "api-reference-v2");
        assert_eq!(slugify("Hello World!"), "hello-world");
    }

    #[test]
    fn test_process_multiple_files() {
        let files = vec![
            ("README.md".to_string(), "# Title\n## Section\n".to_string()),
            ("docs/guide.md".to_string(), "# Guide\n[link](README.md)\n".to_string()),
        ];
        let result = process_markdown_files(&files);
        assert_eq!(result.sections, 3);
        assert_eq!(result.links, 1);
    }

    #[test]
    fn test_line_numbers() {
        let content = "line0\nline1\n# Heading\nline3\n";
        let (headings, _) = extract_markdown("test.md", content);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].line, 2);
    }

    #[test]
    fn test_parse_heading_edge_cases() {
        // Not a heading (no space after #)
        assert!(parse_heading("#no-space").is_none());
        // Not a heading (empty)
        assert!(parse_heading("# ").is_none());
        // Not a heading (7 #s)
        assert!(parse_heading("####### too many").is_none());
        // Closing #s stripped
        assert_eq!(parse_heading("## Heading ##"), Some((2, "Heading")));
        // Tab after #
        assert_eq!(parse_heading("#\tTab heading"), Some((1, "Tab heading")));
    }

    #[test]
    fn test_parse_links_edge_cases() {
        let mut links = Vec::new();
        // Multiple links on one line
        parse_links("[a](b.md) and [c](d.md)", 0, "test.md", &mut links);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target_file, "b.md");
        assert_eq!(links[1].target_file, "d.md");

        // Empty brackets
        links.clear();
        parse_links("[](empty.md)", 0, "test.md", &mut links);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target_file, "empty.md");

        // No closing paren
        links.clear();
        parse_links("[text](unclosed", 0, "test.md", &mut links);
        assert!(links.is_empty());
    }

    #[test]
    fn test_heading_with_special_chars() {
        let (headings, _) = extract_markdown("test.md", "# Hello & World!\n## C++ Guide\n");
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].text, "Hello & World!");
        assert_eq!(headings[0].slug, "hello-world");
        assert_eq!(headings[1].text, "C++ Guide");
        assert_eq!(headings[1].slug, "c-guide");
    }
}
