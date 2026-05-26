pub mod evidence;
pub mod orm;
pub mod markdown;
pub mod cobol;
pub mod reference_index;
pub mod registries;

pub use evidence::*;
pub use orm::*;
pub use markdown::*;
pub use cobol::*;
pub use reference_index::*;
pub use registries::*;

use serde::{Serialize, Deserialize};
use crate::lang::{LanguageId, CaptureTag};
use crate::syntax::RawCapture;

// =====================================================================
// Confidence-scored inference tiers
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    ExactLocal,       // direct local symbol in same scope
    ExactImport,      // resolved via explicit import
    ExactExport,      // resolved via re-export chain
    AnnotationInferred, // from type annotation (e.g., `x: Foo`)
    ConstructorInferred, // from `new Foo()` or `Foo()`
    ReceiverHeuristic, // `self.method()` / `this.method()` receiver chain
    GlobalFallback,   // matched by name across all indexed symbols
    Ambiguous,        // multiple candidates, can't disambiguate
    Unresolved,       // no candidate found
}

impl Confidence {
    pub fn score(&self) -> f64 {
        match self {
            Confidence::ExactLocal => 1.0,
            Confidence::ExactImport => 0.95,
            Confidence::ExactExport => 0.95,
            Confidence::AnnotationInferred => 0.85,
            Confidence::ConstructorInferred => 0.80,
            Confidence::ReceiverHeuristic => 0.70,
            Confidence::GlobalFallback => 0.45,
            Confidence::Ambiguous => 0.30,
            Confidence::Unresolved => 0.0,
        }
    }
}

// =====================================================================
// Symbol IR
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Symbol {
    pub id: u64,
    pub name: String,
    pub qualified_name: String,
    pub kind: CaptureTag,
    pub file_id: u64,
    pub scope_id: Option<u64>,
    pub owner_id: Option<u64>,
    pub line: usize,
    pub col: usize,
    pub is_exported: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Scope {
    pub id: u64,
    pub file_id: u64,
    pub parent_id: Option<u64>,
    pub owner_symbol_id: Option<u64>,
    pub kind: ScopeKind,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeKind {
    Module,
    Function,
    Class,
    Interface,
    Struct,
    Enum,
    Trait,
    Impl,
    Block,
    Namespace,
    Method,
    Constructor,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Import {
    pub file_id: u64,
    pub source: String,       // raw import path
    pub imported_name: String, // the name being imported
    pub local_name: String,    // local alias (may == imported_name)
    pub resolved_file_id: Option<u64>,
    pub confidence: Confidence,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Export {
    pub file_id: u64,
    pub exported_name: String,
    pub symbol_id: u64,
    pub is_default: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Reference {
    pub file_id: u64,
    pub scope_id: Option<u64>,
    pub name: String,
    pub resolved_symbol_id: Option<u64>,
    pub confidence: Confidence,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Call {
    pub file_id: u64,
    pub caller_scope_id: Option<u64>,
    pub callee_name: String,
    pub receiver: Option<String>, // e.g., "self", "this", or inferred type
    pub call_form: crate::syntax::CallForm, // classified at parse time
    pub resolved_symbol_id: Option<u64>,
    pub confidence: Confidence,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Heritage {
    pub file_id: u64,
    pub class_name: String,
    pub heritage_kind: HeritageKind,
    pub target_name: String,
    pub resolved_symbol_id: Option<u64>,
    pub confidence: Confidence,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeritageKind {
    Extends,
    Implements,
    UsesTrait,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Assignment {
    pub file_id: u64,
    pub name: String,
    pub receiver: Option<String>,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Decorator {
    pub file_id: u64,
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HttpClient {
    pub file_id: u64,
    pub name: String,
    pub url: Option<String>,
    pub line: usize,
}

// =====================================================================
// ParsedFile IR — the intermediate representation per file
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ParsedFile {
    pub id: u64,
    pub path: String,
    pub language: LanguageId,
    pub hash: u64,
    pub symbols: Vec<Symbol>,
    pub scopes: Vec<Scope>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub references: Vec<Reference>,
    pub calls: Vec<Call>,
    pub heritage: Vec<Heritage>,
    pub assignments: Vec<Assignment>,
    pub decorators: Vec<Decorator>,
    pub http_clients: Vec<HttpClient>,
    /// Type bindings extracted from AST: variable/parameter name → type text.
    pub type_bindings: Vec<crate::syntax::TypeBinding>,
}

// =====================================================================
// Legacy flat types for JSON output compatibility
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Def {
    pub name: String,
    pub tag: CaptureTag,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CallSite {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HeritageRef {
    pub tag: CaptureTag,
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AssignmentRef {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DecoratorRef {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HttpClientRef {
    pub name: String,
    pub line: usize,
}

/// Flat representation for JSON output (backward-compatible with schema v2)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ParsedFileOutput {
    pub path: String,
    pub language: LanguageId,
    pub defs: Vec<Def>,
    pub calls: Vec<CallSite>,
    pub imports: Vec<String>,
    pub heritage: Vec<HeritageRef>,
    pub assignments: Vec<AssignmentRef>,
    pub decorators: Vec<DecoratorRef>,
    pub http_clients: Vec<HttpClientRef>,
    pub type_bindings: Vec<crate::syntax::TypeBinding>,
}

// =====================================================================
// Raw capture → ParsedFile IR (first pass, no resolution yet)
// =====================================================================

impl ParsedFile {
    pub fn from_captures(id: u64, path: &str, lang: LanguageId, hash: u64, captures: Vec<RawCapture>) -> Self {
        Self::from_captures_with_scopes(id, path, lang, hash, captures, Vec::new(), Vec::new())
    }

    pub fn from_captures_with_scopes(
        id: u64, path: &str, lang: LanguageId, hash: u64,
        captures: Vec<RawCapture>,
        raw_scopes: Vec<crate::syntax::RawScope>,
        type_bindings: Vec<crate::syntax::TypeBinding>,
    ) -> Self {
        // Convert RawScope → Scope, assigning globally unique IDs.
        // Use the same scheme as symbol IDs: high 32 bits = file_id, low 32 bits = scope index.
        // This prevents scope ID collisions across files in ScopeResolutionIndexes.
        let mut scopes: Vec<Scope> = raw_scopes
            .iter()
            .enumerate()
            .map(|(idx, rs)| Scope {
                id: ((id & 0xFFFFFFFF) << 32) | (idx as u64 & 0xFFFFFFFF),
                file_id: id,
                parent_id: rs.parent_idx.map(|p| ((id & 0xFFFFFFFF) << 32) | (p as u64 & 0xFFFFFFFF)),
                owner_symbol_id: None, // filled in during symbol processing
                kind: rs.kind,
                line_start: rs.line_start,
                line_end: rs.line_end,
            })
            .collect();

        // Pre-compute scope ownership: for each scope, find its owner symbol index
        // (the symbol that defines the class/struct/trait/impl that owns this scope)
        let mut scope_owner: Vec<Option<usize>> = vec![None; scopes.len()];
        for idx in 0..scopes.len() {
            if matches!(scopes[idx].kind,
                ScopeKind::Class | ScopeKind::Struct | ScopeKind::Trait |
                ScopeKind::Impl | ScopeKind::Interface | ScopeKind::Enum) {
                // This scope's owner will be set when we process its defining symbol
                // For now, mark that this scope IS a class-like scope
                scope_owner[idx] = Some(idx); // self-referential, will be updated
            }
        }

        // Build a map from globally unique scope ID → array index.
        let scope_id_to_idx: rustc_hash::FxHashMap<u64, usize> = scopes
            .iter()
            .enumerate()
            .map(|(idx, s)| (s.id, idx))
            .collect();

        // Pre-compute: for each scope, the index of the innermost class-like ancestor scope
        // that has an owner symbol. This avoids borrow issues during symbol processing.
        let mut scope_class_ancestor: Vec<Option<usize>> = vec![None; scopes.len()];
        for idx in 0..scopes.len() {
            let mut cur = idx;
            loop {
                if let Some(pid) = scopes[cur].parent_id {
                    let parent_idx = scope_id_to_idx[&pid];
                    if parent_idx == cur { break; }
                    if matches!(scopes[parent_idx].kind,
                        ScopeKind::Class | ScopeKind::Struct | ScopeKind::Trait |
                        ScopeKind::Impl | ScopeKind::Interface | ScopeKind::Enum) {
                        scope_class_ancestor[idx] = Some(parent_idx);
                        break;
                    }
                    cur = parent_idx;
                } else {
                    break;
                }
            }
        }

        let mut symbols: Vec<Symbol> = Vec::new();
        let mut imports = Vec::new();
        let exports = Vec::new();
        let references = Vec::new();
        let mut calls = Vec::new();
        let mut heritage = Vec::new();
        let mut assignments = Vec::new();
        let mut decorators = Vec::new();
        let mut http_clients = Vec::new();

        let mut seen_symbols = std::collections::HashSet::<(String, usize)>::new();
        let mut seen_calls = std::collections::HashSet::<(String, usize)>::new();
        let mut seen_imports = std::collections::HashSet::<String>::new();
        let mut seen_heritage = std::collections::HashSet::<(String, usize)>::new();
        let mut seen_assignments = std::collections::HashSet::<(String, usize)>::new();
        let mut seen_decorators = std::collections::HashSet::<(String, usize)>::new();
        let mut seen_http = std::collections::HashSet::<(String, usize)>::new();

        for c in captures {
            let line = c.range.start_point.row;
            let col = c.range.start_point.column;
            match c.tag {
                CaptureTag::DefinitionClass
                | CaptureTag::DefinitionFunction
                | CaptureTag::DefinitionMethod
                | CaptureTag::DefinitionInterface
                | CaptureTag::DefinitionEnum
                | CaptureTag::DefinitionStruct
                | CaptureTag::DefinitionTrait
                | CaptureTag::DefinitionProperty
                | CaptureTag::DefinitionVariable
                | CaptureTag::DefinitionConst
                | CaptureTag::DefinitionModule
                | CaptureTag::DefinitionMacro
                | CaptureTag::DefinitionNamespace
                | CaptureTag::DefinitionConstructor
                | CaptureTag::DefinitionType
                | CaptureTag::DefinitionTypedef
                | CaptureTag::DefinitionUnion
                | CaptureTag::DefinitionTemplate
                | CaptureTag::DefinitionAnnotation
                | CaptureTag::DefinitionStatic
                | CaptureTag::DefinitionImpl
                | CaptureTag::DefinitionRecord
                | CaptureTag::DefinitionDelegate => {
                    let key = (c.name.clone(), line);
                    if seen_symbols.insert(key) {
                        // Find innermost scope containing this line
                        let mut scope_idx: Option<usize> = None;
                        let mut best_size: usize = usize::MAX;
                        for (idx, s) in scopes.iter().enumerate() {
                            if line >= s.line_start && line <= s.line_end {
                                let size = s.line_end - s.line_start;
                                if size < best_size {
                                    scope_idx = Some(idx);
                                    best_size = size;
                                }
                            }
                        }
                        // Use the globally unique scope ID (same scheme as symbol IDs)
                        let scope_id = scope_idx.map(|idx| scopes[idx].id);
                        // Owner: walk up via pre-computed class ancestor, then check owner_symbol_id
                        let owner_id = scope_idx.and_then(|si| {
                            scope_class_ancestor[si].and_then(|anc_idx| {
                                scopes[anc_idx].owner_symbol_id
                            })
                        });
                        let sym_idx = symbols.len();
                        // Unique symbol ID: high 32 bits = file_id hash, low 32 bits = symbol index
                        // This ensures uniqueness within a file and across files
                        let sym_id = ((id & 0xFFFFFFFF) << 32) | (sym_idx as u64 & 0xFFFFFFFF);
                        symbols.push(Symbol {
                            id: sym_id,
                            name: c.name.clone(),
                            qualified_name: c.name.clone(),
                            kind: c.tag,
                            file_id: id,
                            scope_id,
                            owner_id,
                            line,
                            col,
                            is_exported: false,
                        });
                        // If this is a class-like definition, mark it as the owner of its scope
                        if matches!(c.tag,
                            CaptureTag::DefinitionClass | CaptureTag::DefinitionStruct |
                            CaptureTag::DefinitionTrait | CaptureTag::DefinitionImpl |
                            CaptureTag::DefinitionInterface | CaptureTag::DefinitionEnum) {
                            if let Some(si) = scope_idx {
                                scopes[si].owner_symbol_id = Some(sym_id);
                            }
                        }
                    }
                }
                CaptureTag::CallName => {
                    let key = (c.name.clone(), line);
                    if seen_calls.insert(key) {
                        // Find innermost scope containing this call
                        let mut caller_scope_id: Option<u64> = None;
                        let mut best_size: usize = usize::MAX;
                        for s in scopes.iter() {
                            if line >= s.line_start && line <= s.line_end {
                                let size = s.line_end - s.line_start;
                                if size < best_size {
                                    caller_scope_id = Some(s.id);
                                    best_size = size;
                                }
                            }
                        }
                        calls.push(Call {
                            file_id: id,
                            caller_scope_id,
                            callee_name: c.name,
                            receiver: c.receiver,
                            call_form: c.call_form,
                            resolved_symbol_id: None,
                            confidence: Confidence::Unresolved,
                            line,
                            col,
                        });
                    }
                }
                CaptureTag::ImportSource => {
                    let cleaned = c.name.trim_matches(|ch| ch == '\'' || ch == '"').to_string();
                    if seen_imports.insert(cleaned.clone()) {
                        imports.push(Import {
                            file_id: id,
                            source: cleaned.clone(),
                            imported_name: cleaned.clone(),
                            local_name: cleaned.clone(),
                            resolved_file_id: None,
                            confidence: Confidence::Unresolved,
                        });
                    }
                }
                CaptureTag::HeritageExtends
                | CaptureTag::HeritageImplements
                | CaptureTag::HeritageTrait => {
                    let key = (c.name.clone(), line);
                    if seen_heritage.insert(key) {
                        let hkind = match c.tag {
                            CaptureTag::HeritageExtends => HeritageKind::Extends,
                            CaptureTag::HeritageImplements => HeritageKind::Implements,
                            CaptureTag::HeritageTrait => HeritageKind::UsesTrait,
                            _ => HeritageKind::Unknown,
                        };
                        heritage.push(Heritage {
                            file_id: id,
                            class_name: String::new(), // filled by resolver
                            heritage_kind: hkind,
                            target_name: c.name,
                            resolved_symbol_id: None,
                            confidence: Confidence::Unresolved,
                            line,
                        });
                    }
                }
                CaptureTag::Assignment => {
                    let key = (c.name.clone(), line);
                    if seen_assignments.insert(key) {
                        assignments.push(Assignment {
                            file_id: id,
                            name: c.name,
                            receiver: None,
                            line,
                        });
                    }
                }
                CaptureTag::Decorator => {
                    let key = (c.name.clone(), line);
                    if seen_decorators.insert(key) {
                        decorators.push(Decorator {
                            file_id: id,
                            name: c.name,
                            line,
                        });
                    }
                }
                CaptureTag::HttpClient => {
                    let key = (c.name.clone(), line);
                    if seen_http.insert(key) {
                        http_clients.push(HttpClient {
                            file_id: id,
                            name: c.name,
                            url: None,
                            line,
                        });
                    }
                }
                CaptureTag::Unknown | CaptureTag::CallWrapper | CaptureTag::ImportWrapper | CaptureTag::HeritageWrapper | CaptureTag::TypeAnnotation => {}
            }
        }

        Self {
            id,
            path: path.to_string(),
            language: lang,
            hash,
            symbols,
            scopes,
            imports,
            exports,
            references,
            calls,
            heritage,
            assignments,
            decorators,
            http_clients,
            type_bindings,
        }
    }

    /// Convert to flat output format for JSON serialization
    pub fn to_output(&self) -> ParsedFileOutput {
        ParsedFileOutput {
            path: self.path.clone(),
            language: self.language,
            defs: self.symbols.iter().map(|s| Def {
                name: s.name.clone(),
                tag: s.kind,
                line: s.line,
            }).collect(),
            calls: self.calls.iter().map(|c| CallSite {
                name: c.callee_name.clone(),
                line: c.line,
            }).collect(),
            imports: self.imports.iter().map(|i| i.source.clone()).collect(),
            heritage: self.heritage.iter().map(|h| HeritageRef {
                tag: match h.heritage_kind {
                    HeritageKind::Extends => CaptureTag::HeritageExtends,
                    HeritageKind::Implements => CaptureTag::HeritageImplements,
                    HeritageKind::UsesTrait => CaptureTag::HeritageTrait,
                    HeritageKind::Unknown => CaptureTag::Unknown,
                },
                name: h.target_name.clone(),
                line: h.line,
            }).collect(),
            assignments: self.assignments.iter().map(|a| AssignmentRef {
                name: a.name.clone(),
                line: a.line,
            }).collect(),
            decorators: self.decorators.iter().map(|d| DecoratorRef {
                name: d.name.clone(),
                line: d.line,
            }).collect(),
            http_clients: self.http_clients.iter().map(|h| HttpClientRef {
                name: h.name.clone(),
                line: h.line,
            }).collect(),
            type_bindings: self.type_bindings.clone(),
        }
    }
}
