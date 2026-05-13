use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};
use crate::lang::{LanguageProvider, CaptureTag};

pub struct SyntaxEngine;

pub struct RawCapture {
    pub tag: CaptureTag,
    pub name: String,
    pub range: tree_sitter::Range,
}

impl SyntaxEngine {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_captures(&mut self, provider: &dyn LanguageProvider, content: &str) -> Vec<RawCapture> {
        let mut parser = Parser::new();
        if parser.set_language(&provider.tree_sitter_language()).is_err() {
            return Vec::new();
        }

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => {
                return Vec::new();
            }
        };

        let query = match Query::new(&provider.tree_sitter_language(), provider.query()) {
            Ok(q) => q,
            Err(_) => {
                return Vec::new();
            }
        };

        let capture_names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

        let mut captures = Vec::new();
        let mut seen = std::collections::HashSet::<(String, usize, usize)>::new();

        while let Some(m) = matches.next() {
            // Collect all captures in this match with their metadata.
            // Keep @name captures separate — they provide the identifier text.
            let mut name_capture_idx: Option<usize> = None;
            let mut semantic_captures: Vec<(usize, CaptureTag)> = Vec::new();

            for c in m.captures.iter() {
                let tag_name = capture_names[c.index as usize];
                let tag = CaptureTag::from(tag_name);
                if tag_name == "name" {
                    // Plain @name — just the identifier text, no semantic meaning
                    name_capture_idx = Some(c.index as usize);
                } else if tag_name.ends_with(".name") {
                    // @call.name etc. — serves as BOTH name text and semantic tag
                    name_capture_idx = Some(c.index as usize);
                    if tag != CaptureTag::Unknown {
                        semantic_captures.push((c.index as usize, tag));
                    }
                } else if tag != CaptureTag::Unknown {
                    semantic_captures.push((c.index as usize, tag));
                }
            }

            if let Some(name_idx) = name_capture_idx {
                // Get the name text from the @name capture node
                let name_capture = m
                    .captures
                    .iter()
                    .find(|c| c.index as usize == name_idx)
                    .unwrap();
                let name_text =
                    &content[name_capture.node.start_byte()..name_capture.node.end_byte()];
                let name_range = name_capture.node.range();

                // Pair the name with all semantic tags in this match.
                // Skip wrapper tags (CallWrapper, ImportWrapper, HeritageWrapper) —
                // they're redundant when we have the specific tag (CallName, etc.).
                for &(_, ref tag) in &semantic_captures {
                    match tag {
                        CaptureTag::CallWrapper
                        | CaptureTag::ImportWrapper
                        | CaptureTag::HeritageWrapper => continue,
                        _ => {}
                    }
                    let key = (
                        name_text.to_string(),
                        name_range.start_byte,
                        name_range.end_byte,
                    );
                    if seen.insert(key) {
                        captures.push(RawCapture {
                            tag: *tag,
                            name: name_text.to_string(),
                            range: name_range,
                        });
                    }
                }
            } else {
                // No @name capture — use the capture text directly
                // (import.source, heritage.*, decorator, http_client, assignment)
                for &(idx, ref tag) in &semantic_captures {
                    let c = m.captures.iter().find(|c| c.index as usize == idx).unwrap();
                    let text = &content[c.node.start_byte()..c.node.end_byte()];
                    let key = (text.to_string(), c.node.start_byte(), c.node.end_byte());
                    if seen.insert(key) {
                        captures.push(RawCapture {
                            tag: *tag,
                            name: text.to_string(),
                            range: c.node.range(),
                        });
                    }
                }
            }
        }
        captures
    }
}
