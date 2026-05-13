use serde::{Serialize, Deserialize};
use crate::lang::{LanguageId, CaptureTag};
use crate::syntax::RawCapture;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Def {
    pub name: String,
    pub tag: CaptureTag,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Call {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ParsedFile {
    pub path: String,
    pub language: LanguageId,
    pub defs: Vec<Def>,
    pub calls: Vec<Call>,
    pub imports: Vec<String>,
}

impl ParsedFile {
    pub fn from_captures(path: &str, lang: LanguageId, captures: Vec<RawCapture>) -> Self {
        let mut defs = Vec::new();
        let mut calls = Vec::new();
        let mut imports = Vec::new();
        let mut seen_defs = std::collections::HashSet::<(String, u32)>::new();
        let mut seen_calls = std::collections::HashSet::<(String, u32)>::new();
        let mut seen_imports = std::collections::HashSet::<String>::new();

        for c in captures {
            match c.tag {
                CaptureTag::DefinitionClass | CaptureTag::DefinitionFunction | CaptureTag::DefinitionMethod => {
                    let line = c.range.start_point.row as u32;
                    let key = (c.name.clone(), line);
                    if seen_defs.insert(key) {
                        defs.push(Def {
                            name: c.name,
                            tag: c.tag,
                            line: c.range.start_point.row,
                        });
                    }
                },
                CaptureTag::CallName => {
                    let line = c.range.start_point.row as u32;
                    let key = (c.name.clone(), line);
                    if seen_calls.insert(key) {
                        calls.push(Call {
                            name: c.name,
                            line: c.range.start_point.row,
                        });
                    }
                },
                CaptureTag::ImportSource => {
                    let cleaned = c.name.trim_matches(|c| c == '\'' || c == '\"').to_string();
                    if seen_imports.insert(cleaned.clone()) {
                        imports.push(cleaned);
                    }
                },
                _ => {}
            }
        }

        Self {
            path: path.to_string(),
            language: lang,
            defs,
            calls,
            imports,
        }
    }
}
