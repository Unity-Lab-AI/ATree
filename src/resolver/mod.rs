use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use crate::semantic::{ParsedFile, Symbol, Call, Heritage, Confidence, Import, Export};

// =====================================================================
// SymbolLocation — where a symbol is defined
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SymbolLocation {
    pub file_id: u64,
    pub file_path: String,
    pub line: usize,
    pub col: usize,
}

// =====================================================================
// ResolutionEdge — a resolved connection between symbols
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ResolutionEdge {
    pub src_id: u64,
    pub dst_id: u64,
    pub edge_kind: EdgeKind,
    pub confidence: Confidence,
    pub file_id: u64,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeKind {
    Calls,
    Extends,
    Implements,
    UsesTrait,
    Imports,
    Exports,
    References,
    Assigns,
    Decorates,
    HttpRequest,
}

// =====================================================================
// SemanticModel — the in-memory hot-path code intelligence index
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SemanticModel {
    // File index
    pub files: HashMap<u64, ParsedFile>,
    pub files_by_path: HashMap<String, u64>,

    // Symbol indexes
    pub symbols: HashMap<u64, Symbol>,
    pub symbols_by_name: HashMap<String, Vec<u64>>,       // name → [symbol_id]
    pub symbols_by_file: HashMap<u64, Vec<u64>>,          // file_id → [symbol_id]
    pub next_symbol_id: u64,

    // Resolution edges
    pub edges: Vec<ResolutionEdge>,
    pub edges_by_src: HashMap<u64, Vec<usize>>,           // symbol_id → [edge_index]
    pub edges_by_dst: HashMap<u64, Vec<usize>>,           // symbol_id → [edge_index]

    // Import/Export resolution
    pub imports_by_file: HashMap<u64, Vec<Import>>,
    pub exports_by_name: HashMap<String, Vec<Export>>,     // exported_name → [Export]

    // Stats
    pub total_files: usize,
    pub total_symbols: usize,
    pub total_calls: usize,
    pub total_resolved: usize,
    pub total_unresolved: usize,
}

impl SemanticModel {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            files_by_path: HashMap::new(),
            symbols: HashMap::new(),
            symbols_by_name: HashMap::new(),
            symbols_by_file: HashMap::new(),
            next_symbol_id: 1,
            edges: Vec::new(),
            edges_by_src: HashMap::new(),
            edges_by_dst: HashMap::new(),
            imports_by_file: HashMap::new(),
            exports_by_name: HashMap::new(),
            total_files: 0,
            total_symbols: 0,
            total_calls: 0,
            total_resolved: 0,
            total_unresolved: 0,
        }
    }

    /// Ingest all parsed files, assign symbol IDs, build indexes, run resolution.
    pub fn build_from_parsed(files: Vec<ParsedFile>) -> Self {
        let mut model = Self::new();

        // Phase 1: Index files and assign symbol IDs
        for file in files {
            let file_id = file.id;
            model.files_by_path.insert(file.path.clone(), file_id);
            model.imports_by_file.insert(file_id, file.imports.clone());

            // Assign IDs to symbols and index them
            let mut file_symbol_ids = Vec::new();
            for sym in &file.symbols {
                let id = model.next_symbol_id;
                model.next_symbol_id += 1;
                file_symbol_ids.push(id);
                model.symbols_by_name
                    .entry(sym.name.clone())
                    .or_insert_with(Vec::new)
                    .push(id);
                model.symbols.insert(id, Symbol {
                    id,
                    ..sym.clone()
                });
            }
            model.symbols_by_file.insert(file_id, file_symbol_ids);
            model.total_symbols += file.symbols.len();
            model.total_calls += file.calls.len();

            // Index exports
            for exp in &file.exports {
                model.exports_by_name
                    .entry(exp.exported_name.clone())
                    .or_insert_with(Vec::new)
                    .push(exp.clone());
            }

            model.files.insert(file_id, file);
            model.total_files += 1;
        }

        // Phase 2: Run resolver passes
        model.resolve_all();

        model
    }

    fn resolve_all(&mut self) {
        self.resolve_imports();
        self.resolve_calls();
        self.resolve_heritage();
        self.compute_stats();
    }

    /// Pass 1: Resolve imports to target files
    fn resolve_imports(&mut self) {
        let file_ids: Vec<u64> = self.files.keys().copied().collect();
        for file_id in file_ids {
            let imports = self.imports_by_file.get(&file_id).cloned().unwrap_or_default();
            for import in &imports {
                // Try to match import source to a known file path
                let resolved = self.files_by_path.iter()
                    .find(|(path, _)| {
                        path.ends_with(&import.source) ||
                        path.contains(&import.source) ||
                        import.source.contains(*path)
                    })
                    .map(|(_, fid)| *fid);

                if let Some(target_file) = resolved {
                    if let Some(file) = self.files.get_mut(&file_id) {
                        for imp in &mut file.imports {
                            if imp.source == import.source {
                                imp.resolved_file_id = Some(target_file);
                                imp.confidence = Confidence::ExactImport;
                            }
                        }
                    }
                    // Update the import in our index
                    if let Some(imports) = self.imports_by_file.get_mut(&file_id) {
                        for imp in imports {
                            if imp.source == import.source {
                                imp.resolved_file_id = Some(target_file);
                                imp.confidence = Confidence::ExactImport;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Pass 2: Resolve calls to symbol definitions with confidence scoring
    fn resolve_calls(&mut self) {
        let file_ids: Vec<u64> = self.files.keys().copied().collect();
        for file_id in file_ids {
            let calls: Vec<Call> = self.files.get(&file_id)
                .map(|f| f.calls.clone())
                .unwrap_or_default();

            for (call_idx, call) in calls.iter().enumerate() {
                let resolution = self.resolve_call(call);
                let (resolved_id, resolved_conf, resolved_recv) = match resolution {
                    Some((sid, conf, recv)) => (Some(sid), conf, recv),
                    None => (None, Confidence::Unresolved, None),
                };

                // Update the call in the file
                if let Some(file) = self.files.get_mut(&file_id) {
                    if call_idx < file.calls.len() {
                        file.calls[call_idx].resolved_symbol_id = resolved_id;
                        file.calls[call_idx].confidence = resolved_conf;
                        file.calls[call_idx].receiver = resolved_recv;
                    }
                }

                // Create resolution edge
                if let Some(sym_id) = resolved_id {
                    let confidence = resolved_conf;
                    let edge_idx = self.edges.len();
                    self.edges.push(ResolutionEdge {
                        src_id: 0, // call site doesn't have a symbol id
                        dst_id: sym_id,
                        edge_kind: EdgeKind::Calls,
                        confidence,
                        file_id,
                        line: call.line,
                    });
                    self.edges_by_dst.entry(sym_id).or_insert_with(Vec::new).push(edge_idx);
                }
            }
        }
    }

    /// Resolve a single call: same-file → import → global fallback
    fn resolve_call(&self, call: &Call) -> Option<(u64, Confidence, Option<String>)> {
        let file_id = call.file_id;
        let callee = &call.callee_name;

        // Strategy 1: Exact local — same file defines it
        if let Some(sym_ids) = self.symbols_by_file.get(&file_id) {
            for sym_id in sym_ids {
                let sym = self.symbols.get(sym_id)?;
                if sym.name == *callee {
                    return Some((*sym_id, Confidence::ExactLocal, None));
                }
            }
        }

        // Strategy 2: Import resolution — check if this name was imported
        if let Some(imports) = self.imports_by_file.get(&file_id) {
            for imp in imports {
                if imp.imported_name == *callee || imp.local_name == *callee {
                    // Try to find the symbol in the resolved file
                    if let Some(target_file) = imp.resolved_file_id {
                        if let Some(sym_ids) = self.symbols_by_file.get(&target_file) {
                            for sym_id in sym_ids {
                                let sym = self.symbols.get(sym_id)?;
                                if sym.name == *callee {
                                    return Some((*sym_id, Confidence::ExactImport, None));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Strategy 3: Receiver heuristic — self.method() / this.method()
        if let Some(ref receiver) = call.receiver {
            // Try to find a method on the receiver type
            if let Some(sym_ids) = self.symbols_by_name.get(callee) {
                for sym_id in sym_ids {
                    let sym = self.symbols.get(sym_id)?;
                    if sym.owner_id.is_some() {
                        return Some((*sym_id, Confidence::ReceiverHeuristic, Some(receiver.clone())));
                    }
                }
            }
        }

        // Strategy 4: Global fallback — match by name across all symbols
        if let Some(sym_ids) = self.symbols_by_name.get(callee) {
            if sym_ids.len() == 1 {
                return Some((sym_ids[0], Confidence::GlobalFallback, None));
            } else if sym_ids.len() > 1 {
                // Ambiguous — multiple candidates
                return Some((sym_ids[0], Confidence::Ambiguous, None));
            }
        }

        None
    }

    /// Pass 3: Resolve heritage (extends/implements) to symbol definitions
    fn resolve_heritage(&mut self) {
        let file_ids: Vec<u64> = self.files.keys().copied().collect();
        for file_id in file_ids {
            let heritage: Vec<Heritage> = self.files.get(&file_id)
                .map(|f| f.heritage.clone())
                .unwrap_or_default();

            for (hidx, h) in heritage.iter().enumerate() {
                let resolution = self.resolve_heritage_ref(h);

                if let Some(file) = self.files.get_mut(&file_id) {
                    if hidx < file.heritage.len() {
                        file.heritage[hidx].resolved_symbol_id = resolution.map(|(sid, _)| sid);
                        file.heritage[hidx].confidence = resolution.map(|(_, conf)| conf).unwrap_or(Confidence::Unresolved);
                    }
                }

                if let Some((sym_id, confidence)) = resolution {
                    let edge_idx = self.edges.len();
                    let edge_kind = match h.heritage_kind {
                        crate::semantic::HeritageKind::Extends => EdgeKind::Extends,
                        crate::semantic::HeritageKind::Implements => EdgeKind::Implements,
                        crate::semantic::HeritageKind::UsesTrait => EdgeKind::UsesTrait,
                        _ => EdgeKind::References,
                    };
                    self.edges.push(ResolutionEdge {
                        src_id: 0,
                        dst_id: sym_id,
                        edge_kind,
                        confidence,
                        file_id,
                        line: h.line,
                    });
                    self.edges_by_dst.entry(sym_id).or_insert_with(Vec::new).push(edge_idx);
                }
            }
        }
    }

    fn resolve_heritage_ref(&self, h: &Heritage) -> Option<(u64, Confidence)> {
        let target = &h.target_name;

        // Same-file class definition
        if let Some(sym_ids) = self.symbols_by_file.get(&h.file_id) {
            for sym_id in sym_ids {
                let sym = self.symbols.get(sym_id)?;
                if sym.name == *target {
                    return Some((*sym_id, Confidence::ExactLocal));
                }
            }
        }

        // Global fallback
        if let Some(sym_ids) = self.symbols_by_name.get(target) {
            if sym_ids.len() == 1 {
                return Some((sym_ids[0], Confidence::GlobalFallback));
            } else if sym_ids.len() > 1 {
                return Some((sym_ids[0], Confidence::Ambiguous));
            }
        }

        None
    }

    fn compute_stats(&mut self) {
        for file in self.files.values() {
            for call in &file.calls {
                match call.confidence {
                    Confidence::Unresolved => self.total_unresolved += 1,
                    _ => self.total_resolved += 1,
                }
            }
        }
    }

    /// Look up a symbol by name. Returns all matching symbol IDs.
    pub fn lookup_symbol(&self, name: &str) -> Option<&Vec<u64>> {
        self.symbols_by_name.get(name)
    }

    /// Look up where a symbol is defined.
    pub fn symbol_location(&self, symbol_id: u64) -> Option<SymbolLocation> {
        let sym = self.symbols.get(&symbol_id)?;
        let file = self.files.get(&sym.file_id)?;
        Some(SymbolLocation {
            file_id: sym.file_id,
            file_path: file.path.clone(),
            line: sym.line,
            col: sym.col,
        })
    }

    /// Get all edges pointing to a symbol (callers, implementers, etc.)
    pub fn incoming_edges(&self, symbol_id: u64) -> Option<&Vec<usize>> {
        self.edges_by_dst.get(&symbol_id)
    }

    /// Get all edges from a symbol (what it calls, extends, etc.)
    pub fn outgoing_edges(&self, symbol_id: u64) -> Option<&Vec<usize>> {
        self.edges_by_src.get(&symbol_id)
    }
}

// =====================================================================
// SymbolTable — flat name→location index (for JSON output compat)
// =====================================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SymbolTable {
    pub definitions: HashMap<String, Vec<SymbolLocation>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self { definitions: HashMap::new() }
    }

    pub fn from_model(model: &SemanticModel) -> Self {
        let mut table = Self::new();
        for (name, sym_ids) in &model.symbols_by_name {
            let locations: Vec<SymbolLocation> = sym_ids.iter()
                .filter_map(|id| model.symbol_location(*id))
                .collect();
            if !locations.is_empty() {
                table.definitions.insert(name.clone(), locations);
            }
        }
        table
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
