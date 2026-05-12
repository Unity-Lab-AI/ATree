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

        for c in captures {
            match c.tag {
                CaptureTag::DefinitionClass | CaptureTag::DefinitionFunction | CaptureTag::DefinitionMethod => {
                    defs.push(Def {
                        name: c.name,
                        tag: c.tag,
                        line: c.range.start_point.row,
                    });
                },
                CaptureTag::CallName => {
                    calls.push(Call {
                        name: c.name,
                        line: c.range.start_point.row,
                    });
                },
                CaptureTag::ImportSource => {
                    imports.push(c.name.trim_matches(|c| c == '\'' || c == '\"').to_string());
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
