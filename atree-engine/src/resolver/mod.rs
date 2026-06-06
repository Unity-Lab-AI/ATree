//! Scope-aware resolution engine.
//!
//! Ported from GitNexus's scope-resolution pipeline (RFC #909).
//! Architecture:
//!   1. Build scope trees from tree-sitter ASTs (per-file, parallel)
//!   2. Resolve imports (3-tier: same-file → import-scoped → global)
//!   3. Build MRO (first-wins, C3 linearization)
//!   4. Resolve calls (scope-chain walk → receiver inference → dispatch → edge emission)
//!   5. Emit edges to GraphStore

pub mod import_resolver;
pub mod c3;
pub mod import_graph;

use crate::store::{GraphStore, SymbolRecord, ScopeRecord, ImportRecord, CallRecord, EdgeRecord};
use crate::semantic::Confidence;
use rustc_hash::FxHashMap;
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

// Re-export
pub use crate::store::{FileRecord, StoreStats};

/// Per-language import resolution strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStrategy {
    /// Only explicitly imported names visible (TS, JS, Java, C#, Rust, PHP, Kotlin)
    Named,
    /// Whole-package import, no transitive re-exports (Go, Ruby, Swift)
    WildcardLeaf,
    /// #include closure chains (C, C++)
    WildcardTransitive,
    /// Module aliases at call site (Python)
    Namespace,
}

/// Per-language MRO strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MroStrategy {
    FirstWins,
    C3,
    RubyMixin,
    None,
}

/// Language configuration for resolution.
pub struct LanguageConfig {
    pub id: String,
    pub import_strategy: ImportStrategy,
    pub mro_strategy: MroStrategy,
}

/// The resolution engine. Operates on a GraphStore.
pub struct ResolutionEngine<'a> {
    store: &'a GraphStore,
    lang_configs: HashMap<String, LanguageConfig>,
    // Hot-path in-memory indexes (built from store)
    symbols_by_name: FxHashMap<String, Vec<i64>>,
    symbols_by_file: FxHashMap<i64, Vec<i64>>,
    symbols_by_id: FxHashMap<i64, SymbolRecord>, // O(1) symbol lookup by ID
    files_by_path: FxHashMap<String, i64>,
    file_languages: FxHashMap<i64, String>, // file_id → language string
    scopes_by_file: FxHashMap<i64, Vec<ScopeRecord>>,
    imports_by_file: FxHashMap<i64, Vec<ImportRecord>>,
}

impl<'a> ResolutionEngine<'a> {
    pub fn new(store: &'a GraphStore) -> rusqlite::Result<Self> {
        let mut engine = Self {
            store,
            lang_configs: HashMap::new(),
            symbols_by_name: FxHashMap::default(),
            symbols_by_file: FxHashMap::default(),
            symbols_by_id: FxHashMap::default(),
            files_by_path: FxHashMap::default(),
            file_languages: FxHashMap::default(),
            scopes_by_file: FxHashMap::default(),
            imports_by_file: FxHashMap::default(),
        };
        engine.build_indexes()?;
        engine.register_default_languages();
        Ok(engine)
    }

    fn build_indexes(&mut self) -> rusqlite::Result<()> {
        // Index files
        for file in self.store.get_all_files()? {
            self.files_by_path.insert(file.path.clone(), file.id);
            self.file_languages.insert(file.id, file.language.clone());
        }
        // Index symbols
        for file_id in self.files_by_path.values() {
            let symbols = self.store.get_symbols_by_file(*file_id)?;
            let mut sym_ids = Vec::new();
            for sym in &symbols {
                self.symbols_by_name
                    .entry(sym.name.clone())
                    .or_default()
                    .push(sym.id);
                sym_ids.push(sym.id);
            }
            self.symbols_by_file.insert(*file_id, sym_ids);
            // Index each symbol by ID for O(1) lookup
            for sym in &symbols {
                self.symbols_by_id.insert(sym.id, sym.clone());
            }
        }
        // Index scopes
        for file_id in self.files_by_path.values() {
            let scopes = self.store.get_scopes_by_file(*file_id)?;
            self.scopes_by_file.insert(*file_id, scopes);
        }
        // Index imports
        for file_id in self.files_by_path.values() {
            let imports = self.store.get_imports_by_file(*file_id)?;
            self.imports_by_file.insert(*file_id, imports);
        }
        Ok(())
    }

    fn register_default_languages(&mut self) {
        let configs = [
            ("typescript", ImportStrategy::Named, MroStrategy::FirstWins),
            ("javascript", ImportStrategy::Named, MroStrategy::FirstWins),
            ("python", ImportStrategy::Namespace, MroStrategy::C3),
            ("go", ImportStrategy::WildcardLeaf, MroStrategy::FirstWins),
            ("rust", ImportStrategy::Named, MroStrategy::FirstWins),
            ("java", ImportStrategy::Named, MroStrategy::FirstWins),
            ("c", ImportStrategy::WildcardTransitive, MroStrategy::FirstWins),
            ("cpp", ImportStrategy::WildcardTransitive, MroStrategy::FirstWins),
            ("csharp", ImportStrategy::Named, MroStrategy::FirstWins),
            ("php", ImportStrategy::Named, MroStrategy::FirstWins),
            ("ruby", ImportStrategy::WildcardLeaf, MroStrategy::RubyMixin),
            ("kotlin", ImportStrategy::Named, MroStrategy::FirstWins),
            ("swift", ImportStrategy::WildcardLeaf, MroStrategy::FirstWins),
            ("bash", ImportStrategy::Named, MroStrategy::None),
            ("json", ImportStrategy::Named, MroStrategy::None),
            ("yaml", ImportStrategy::Named, MroStrategy::None),
        ];
        for (id, imp, mro) in &configs {
            self.lang_configs.insert(id.to_string(), LanguageConfig {
                id: id.to_string(),
                import_strategy: *imp,
                mro_strategy: *mro,
            });
        }
    }

    // =================================================================
    // Phase 1: Import resolution (3-tier)
    // =================================================================

    pub fn resolve_imports(&self) -> rusqlite::Result<usize> {
        let mut resolved = 0;
        for imports in self.imports_by_file.values() {
            for import in imports {
                if import.resolved_file_id.is_some() {
                    continue; // already resolved
                }
                let target = self.resolve_import_target(import);
                if let Some(target_id) = target {
                    self.store.update_import_resolution(import.id, Some(target_id), Confidence::ExactImport.score())?;
                    resolved += 1;
                }
            }
        }
        Ok(resolved)
    }

    fn resolve_import_target(&self, import: &ImportRecord) -> Option<i64> {
        // Get the language of the importing file
        let lang_str = self.file_languages.get(&import.file_id)?;
        let lang_id = match lang_str.as_str() {
            "TypeScript" => crate::lang::LanguageId::TypeScript,
            "JavaScript" => crate::lang::LanguageId::JavaScript,
            "Python" => crate::lang::LanguageId::Python,
            "Rust" => crate::lang::LanguageId::Rust,
            "Go" => crate::lang::LanguageId::Go,
            "Java" => crate::lang::LanguageId::Java,
            "C" => crate::lang::LanguageId::C,
            "Cpp" => crate::lang::LanguageId::Cpp,
            "CSharp" => crate::lang::LanguageId::CSharp,
            "PHP" => crate::lang::LanguageId::PHP,
            "Ruby" => crate::lang::LanguageId::Ruby,
            "Kotlin" => crate::lang::LanguageId::Kotlin,
            "Swift" => crate::lang::LanguageId::Swift,
            "Bash" => crate::lang::LanguageId::Bash,
            _ => return None,
        };

        // Get the source file path
        let from_path = self.files_by_path.iter()
            .find(|(_, id)| **id == import.file_id)
            .map(|(p, _)| p.as_str())?;

        // Build list of all file paths for resolution
        let all_paths: Vec<String> = self.files_by_path.keys().cloned().collect();

        // Use per-language resolver
        let result = import_resolver::resolve_import(
            &import.source,
            from_path,
            &all_paths,
            lang_id,
        );
        result.and_then(|(resolved_path, _confidence)| {
            let found = self.files_by_path.get(&resolved_path).copied();
            found
        })
    }

    // =================================================================
    // Phase 2: Call resolution (scope-chain walk)
    // =================================================================

    pub fn resolve_calls(&self) -> rusqlite::Result<usize> {
        let mut resolved = 0;
        for file_id in self.symbols_by_file.keys() {
            let calls = self.store.get_calls_by_file(*file_id)?;
            for call in &calls {
                if call.resolved_symbol_id.is_some() {
                    continue;
                }
                let result = self.resolve_call(call);
                if let Some((sym_id, confidence)) = result {
                    self.store.update_call_resolution(call.id, Some(sym_id), confidence.score())?;
                    // Resolve the caller scope to an enclosing symbol (function/method).
                    // caller_scope_id references the scopes table; we need the symbol
                    // that owns this scope (the enclosing function/method).
                    let caller_symbol_id = call.caller_scope_id
                        .and_then(|sid| self.resolve_scope_to_symbol(sid))
                        .unwrap_or(0);
                    if caller_symbol_id > 0 {
                        self.store.insert_edge(&EdgeRecord {
                            id: 0,
                            src_id: caller_symbol_id,
                            dst_id: sym_id,
                            edge_kind: "CALLS".to_string(),
                            confidence: confidence.score(),
                            file_id: Some(*file_id),
                            line: call.line,
                        })?;
                    }
                    resolved += 1;
                }
            }
        }
        Ok(resolved)
    }

    fn resolve_call(&self, call: &CallRecord) -> Option<(i64, Confidence)> {
        let file_id = call.file_id;
        let callee = &call.callee_name;

        // Tier 1: Exact local — same file defines it
        if let Some(sym_ids) = self.symbols_by_file.get(&file_id) {
            for sym_id in sym_ids {
                if let Some(sym) = self.get_symbol(*sym_id) {
                    if sym.name == *callee {
                        return Some((*sym_id, Confidence::ExactLocal));
                    }
                }
            }
        }

        // Tier 2: Import-scoped — check all resolved imports for matching symbols.
        // For each import that resolved to a file, look for symbols in that file
        // matching the callee name. This handles both direct imports (`use foo::bar`
        // calling `bar()`) and module-scoped imports (`use foo::Type` calling
        // `instance.method()` where `method` is defined in `foo`'s module).
        if let Ok(imports) = self.store.get_imports_by_file(file_id) {
            let mut best_import_match: Option<(i64, Confidence)> = None;
            for imp in &imports {
                if let Some(target_file) = imp.resolved_file_id {
                    if let Some(sym_ids) = self.symbols_by_file.get(&target_file) {
                        for sym_id in sym_ids {
                            if let Some(sym) = self.get_symbol(*sym_id) {
                                if sym.name == *callee {
                                    // Prefer exact name match on the import itself
                                    let conf = if imp.imported_name == *callee || imp.local_name == *callee {
                                        Confidence::ExactImport
                                    } else {
                                        Confidence::ImportScoped
                                    };
                                    // Keep best confidence match
                                    if best_import_match.map_or(true, |(_, c)| conf.score() > c.score()) {
                                        best_import_match = Some((*sym_id, conf));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some((sym_id, conf)) = best_import_match {
                return Some((sym_id, conf));
            }
        }

        // Tier 3: Receiver heuristic — self.method() / this.method()
        if call.receiver.is_some() {
            if let Some(sym_ids) = self.symbols_by_name.get(callee) {
                for sym_id in sym_ids {
                    if let Some(sym) = self.get_symbol(*sym_id) {
                        if sym.owner_symbol_id.is_some() {
                            return Some((*sym_id, Confidence::ReceiverHeuristic));
                        }
                    }
                }
            }
        }

        // Tier 4: Global fallback — match by name across all symbols
        if let Some(sym_ids) = self.symbols_by_name.get(callee) {
            if sym_ids.len() == 1 {
                return Some((sym_ids[0], Confidence::GlobalFallback));
            } else if sym_ids.len() > 1 {
                return Some((sym_ids[0], Confidence::Ambiguous));
            }
        }

        None
    }

    fn get_symbol(&self, id: i64) -> Option<SymbolRecord> {
        self.symbols_by_id.get(&id).cloned()
    }

    /// Resolve a scope ID to its owning symbol (the enclosing function/method/class).
    /// Walks up the scope chain until we find a symbol owned by one of the scopes.
    fn resolve_scope_to_symbol(&self, scope_id: i64) -> Option<i64> {
        let mut current = scope_id;
        let mut visited = rustc_hash::FxHashSet::default();
        loop {
            if !visited.insert(current) {
                return None;
            }
            // Check if any symbol is owned by this scope
            if let Some(sym) = self.symbols_by_id.values().find(|s| s.scope_id == Some(current)) {
                return Some(sym.id);
            }
            // Walk up to parent scope
            current = self.scopes_by_file.values()
                .flatten()
                .find(|s| s.id == current)
                .and_then(|s| s.parent_id)?;
        }
    }

    // =================================================================
    // Phase 3: MRO (Method Resolution Order)
    // =================================================================

    /// Build Method Resolution Order (MRO) for all classes.
    /// Resolves heritage relationships into concrete parent symbol IDs
    /// and emits EXTENDS/IMPLEMENTS edges to the graph.
    pub fn build_mro(&self) -> rusqlite::Result<usize> {
        let mut edges = 0;
        let all_heritage = self.store.get_all_heritage()?;

        // Track emitted (child, parent) pairs to avoid duplicates
        let mut emitted: rustc_hash::FxHashSet<(i64, i64)> = rustc_hash::FxHashSet::default();

        for h in &all_heritage {
            // Determine parent symbol ID: use already-resolved value or look up by name
            let parent_id: Option<i64> = if let Some(pid) = h.parent_symbol_id {
                if pid > 0 { Some(pid) } else { None }
            } else if let Some(parent_ids) = self.symbols_by_name.get(&h.parent_name) {
                self.resolve_best_parent(h.child_symbol_id, parent_ids)
            } else {
                None
            };

            if let Some(pid) = parent_id {
                // Skip if we already emitted this edge
                if emitted.insert((h.child_symbol_id, pid)) {
                    let edge_kind = match h.heritage_kind.as_str() {
                        "Implements" | "implements" => "IMPLEMENTS",
                        _ => "EXTENDS",
                    };
                    self.store.insert_edge(&EdgeRecord {
                        id: 0,
                        src_id: h.child_symbol_id,
                        dst_id: pid,
                        edge_kind: edge_kind.to_string(),
                        confidence: 1.0,
                        file_id: None,
                        line: h.line,
                    })?;
                    edges += 1;
                }
            }
        }

        Ok(edges)
    }

    /// Given a child symbol and candidate parent IDs, pick the best parent.
    /// Prefers parents in the same file, then falls back to first match.
    fn resolve_best_parent(&self, child_id: i64, candidate_ids: &[i64]) -> Option<i64> {
        if candidate_ids.len() == 1 {
            return Some(candidate_ids[0]);
        }
        // Get the child's file
        let child_file = self.symbols_by_file.iter()
            .find(|(_, ids)| ids.contains(&child_id))
            .map(|(fid, _)| *fid);

        if let Some(cf) = child_file {
            // Prefer parent in same file
            for pid in candidate_ids {
                if let Some(file_syms) = self.symbols_by_file.get(&cf) {
                    if file_syms.contains(pid) {
                        return Some(*pid);
                    }
                }
            }
        }
        // Fallback: first candidate
        candidate_ids.first().copied()
    }

    // =================================================================
    // Full resolution pipeline
    // =================================================================

    pub fn run_full_resolution(&self) -> rusqlite::Result<ResolutionStats> {
        let imports_resolved = self.resolve_imports()?;
        let calls_resolved = self.resolve_calls()?;
        let mro_edges = self.build_mro()?;

        // Emit DEFINES edges (file → symbol)
        let mut defines_edges = 0;
        for (file_id, sym_ids) in &self.symbols_by_file {
            for sym_id in sym_ids {
                self.store.insert_edge(&EdgeRecord {
                    id: 0,
                    src_id: *file_id,
                    dst_id: *sym_id,
                    edge_kind: "DEFINES".to_string(),
                    confidence: 1.0,
                    file_id: Some(*file_id),
                    line: 0,
                })?;
                defines_edges += 1;
            }
        }

        Ok(ResolutionStats {
            imports_resolved,
            calls_resolved,
            mro_edges,
            defines_edges,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionStats {
    pub imports_resolved: usize,
    pub calls_resolved: usize,
    pub mro_edges: usize,
    pub defines_edges: usize,
}

// =================================================================
// SymbolTable — flat name→location index (for JSON output compat)
// =================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SymbolLocation {
    pub file_id: i64,
    pub file_path: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SymbolTable {
    pub definitions: HashMap<String, Vec<SymbolLocation>>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    pub fn new() -> Self {
        Self { definitions: HashMap::new() }
    }

    pub fn from_store(store: &GraphStore) -> rusqlite::Result<Self> {
        let mut table = Self::new();
        for file in store.get_all_files()? {
            let symbols = store.get_symbols_by_file(file.id)?;
            for sym in symbols {
                table.definitions
                    .entry(sym.name.clone())
                    .or_default()
                    .push(SymbolLocation {
                        file_id: sym.file_id,
                        file_path: file.path.clone(),
                        line: sym.line,
                        col: sym.col,
                    });
            }
        }
        Ok(table)
    }

    pub fn resolve(&self, name: &str) -> Option<&Vec<SymbolLocation>> {
        self.definitions.get(name)
    }

    pub fn len(&self) -> usize {
        self.definitions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }
}
