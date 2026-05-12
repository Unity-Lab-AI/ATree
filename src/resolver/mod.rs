use std::collections::HashMap;
use crate::semantic::{ParsedFile, Def};

pub struct SymbolLocation {
    pub path: String,
    pub line: usize,
}

pub struct SymbolTable {
    pub definitions: HashMap<String, Vec<SymbolLocation>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            definitions: HashMap::new(),
        }
    }

    pub fn index_file(&mut self, file: &ParsedFile) {
        for def in &file.defs {
            self.definitions.entry(def.name.clone())
                .or_insert_with(Vec::new)
                .push(SymbolLocation {
                    path: file.path.clone(),
                    line: def.line,
                });
        }
    }
}
