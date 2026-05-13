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
            None => return Vec::new(),
        };

        let query = match Query::new(&provider.tree_sitter_language(), provider.query()) {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

        let mut captures = Vec::new();
        let mut seen = std::collections::HashSet::<(String, usize, usize)>::new();
        while let Some(m) = matches.next() {
            for c in m.captures {
                let tag_name = query.capture_names()[c.index as usize];
                let tag = CaptureTag::from(tag_name);

                if tag != CaptureTag::Unknown {
                    let node = c.node;
                    let text = &content[node.start_byte()..node.end_byte()];
                    let key = (text.to_string(), node.start_byte(), node.end_byte());
                    if seen.insert(key) {
                        captures.push(RawCapture {
                            tag,
                            name: text.to_string(),
                            range: node.range(),
                        });
                    }
                }
            }
        }
        captures
    }
}
