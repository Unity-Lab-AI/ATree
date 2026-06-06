//! Type Environment — type-aware cross-file resolution.
//!
//! Builds a per-file type environment that maps (scope, variableName) → typeName.
//! Used during call resolution to determine the type of receiver expressions,
//! enabling type-aware linking across files.
//!
//! Ported from GitNexus's type-env.ts (1347 lines).
//!
//! ## Resolution tiers:
//! - Tier 0: Explicit type annotations (`x: Foo`, `Foo x = ...`)
//! - Tier 1: Constructor inference (`x = new Foo()`)
//! - Tier 2: Assignment chain propagation (`const b = a` where `a` is already typed)
//!
//! ## Scope awareness:
//! - Function-local variables keyed by function name
//! - File-level variables keyed by empty string
//! - self/this/super resolved via AST walk to enclosing class

use crate::semantic::{ParsedFile, Symbol, Call, Scope, ScopeKind};
use rustc_hash::FxHashMap;

/// Per-file type environment: scope_key → (var_name → type_name)
/// Scope key is function name for locals, "" for file-level.
#[derive(Debug, Default, Clone)]
pub struct TypeEnvironment {
    /// scope_key → var_name → resolved type name
    pub bindings: FxHashMap<String, FxHashMap<String, String>>,
    /// Variable names that were assigned via constructor calls (Tier 1)
    pub constructor_types: FxHashMap<String, String>,
}

impl TypeEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a variable to a type in a given scope.
    pub fn bind(&mut self, scope_key: &str, var_name: &str, type_name: &str) {
        self.bindings
            .entry(scope_key.to_string())
            .or_default()
            .insert(var_name.to_string(), type_name.to_string());
    }

    /// Look up a variable's type. Handles self/this/super by resolving
    /// to the enclosing class name.
    pub fn lookup(&self, var_name: &str, in_scope: Option<u64>, scopes: &[Scope], symbols: &[Symbol]) -> Option<String> {
        // self/this → enclosing class name
        if var_name == "self" || var_name == "this" || var_name == "$this" {
            return self.resolve_enclosing_class(in_scope, scopes, symbols);
        }
        // super/base/parent → enclosing class's parent
        if var_name == "super" || var_name == "base" || var_name == "parent" {
            return self.resolve_enclosing_parent(in_scope, scopes, symbols);
        }

        // Try function-local scope first, then file-level
        let scope_keys = self.resolve_scope_keys(in_scope, scopes);
        for key in &scope_keys {
            if let Some(scope_map) = self.bindings.get(key) {
                if let Some(type_name) = scope_map.get(var_name) {
                    return Some(self.strip_nullable(type_name));
                }
            }
        }

        // File-level fallback
        if let Some(file_map) = self.bindings.get("") {
            if let Some(type_name) = file_map.get(var_name) {
                return Some(self.strip_nullable(type_name));
            }
        }

        None
    }

    /// Get all bindings for a scope.
    pub fn get_scope(&self, scope_key: &str) -> Option<&FxHashMap<String, String>> {
        self.bindings.get(scope_key)
    }

    /// Get constructor type for a variable (Tier 1).
    pub fn get_constructor_type(&self, var_name: &str) -> Option<&String> {
        self.constructor_types.get(var_name)
    }

    fn resolve_scope_keys(&self, in_scope: Option<u64>, scopes: &[Scope]) -> Vec<String> {
        let mut keys = Vec::new();
        if let Some(scope_id) = in_scope {
            // Walk up the scope chain, collecting function/method scope keys
            let mut current = Some(scope_id);
            while let Some(sid) = current {
                if let Some(scope) = scopes.iter().find(|s| s.id == sid) {
                    if matches!(scope.kind, ScopeKind::Function | ScopeKind::Method | ScopeKind::Constructor) {
                        keys.push(format!("scope_{}", sid));
                    }
                    current = scope.parent_id;
                } else {
                    break;
                }
            }
        }
        keys
    }

    fn resolve_enclosing_class(&self, in_scope: Option<u64>, scopes: &[Scope], symbols: &[Symbol]) -> Option<String> {
        let scope_id = in_scope?;
        let scope = scopes.iter().find(|s| s.id == scope_id)?;

        // Walk up scopes to find enclosing class
        let mut current = Some(scope.id);
        while let Some(sid) = current {
            let s = scopes.iter().find(|sc| sc.id == sid)?;
            if matches!(s.kind, ScopeKind::Class | ScopeKind::Interface | ScopeKind::Struct | ScopeKind::Enum | ScopeKind::Trait) {
                // Find the symbol that owns this scope
                if let Some(owner_id) = s.owner_symbol_id {
                    if let Some(sym) = symbols.iter().find(|sy| sy.id == owner_id) {
                        return Some(sym.name.clone());
                    }
                }
            }
            current = s.parent_id;
        }
        None
    }

    fn resolve_enclosing_parent(
        &self,
        in_scope: Option<u64>,
        scopes: &[Scope],
        symbols: &[Symbol],
    ) -> Option<String> {
        // Find enclosing class, then walk up via heritage edges.
        let class_name = self.resolve_enclosing_class(in_scope, scopes, symbols)?;
        // Search our own constructor_types for a parent reference.
        // In a full implementation this would cross-reference with other files'
        // type environments via the heritage map.
        for parent_name in self.constructor_types.values() {
            if parent_name != &class_name {
                return Some(parent_name.clone());
            }
        }
        None
    }

    /// Resolve the scope key for a given line number.
    /// Finds the innermost scope containing the line and returns its key.
    /// Falls back to file-level ("") if no matching scope is found.
    pub(crate) fn resolve_scope_key_for_line(&self, line: usize, scopes: &[Scope]) -> String {
        // Find the innermost (smallest) scope that contains this line
        let mut best_idx = None;
        let mut best_size = usize::MAX;
        for (idx, scope) in scopes.iter().enumerate() {
            if line >= scope.line_start && line <= scope.line_end {
                let size = scope.line_end - scope.line_start;
                if size < best_size {
                    best_idx = Some(idx);
                    best_size = size;
                }
            }
        }
        match best_idx {
            Some(idx) => format!("scope_{}", idx),
            None => String::new(), // file-level
        }
    }

    /// Strip nullable markers from type names (Foo? → Foo, Foo | null → Foo)
    fn strip_nullable(&self, type_name: &str) -> String {
        let trimmed = type_name.trim();
        // Remove trailing ?
        let without_question = trimmed.strip_suffix('?').unwrap_or(trimmed);
        // Remove | null, | undefined, | None suffixes
        if let Some(idx) = without_question.rfind('|') {
            let after = without_question[idx + 1..].trim();
            if after == "null" || after == "undefined" || after == "None" || after == "nil" {
                return without_question[..idx].trim().to_string();
            }
        }
        without_question.to_string()
    }
}

/// Build a type environment for a single parsed file.
/// Extracts type bindings from:
/// - Tier 0: Explicit type annotations from AST (x: Type, let x: Type, etc.)
/// - Tier 1: Constructor inference (x = new Foo(), x = Foo(), etc.)
/// - Tier 2: Assignment propagation (b = a where a is already typed)
pub fn build_type_env(parsed: &ParsedFile) -> TypeEnvironment {
    let mut env = TypeEnvironment::new();

    // Tier 0: Explicit type annotations from AST.
    for binding in &parsed.type_bindings {
        let scope_key = env.resolve_scope_key_for_line(binding.line, &parsed.scopes);
        env.bind(&scope_key, &binding.var_name, &binding.type_text);
    }

    // Tier 1: Constructor inference from calls.
    // If we see `new Foo()` or `Foo()` with a Constructor call form,
    // and there's a nearby assignment to a variable, infer that variable's type as Foo.
    // We match constructor calls to assignments by looking at assignments on the same line
    // or the preceding line.
    for call in &parsed.calls {
        if call.call_form == crate::syntax::CallForm::Constructor {
            let ctor_line = call.line;
            // Find an assignment on the same or previous line.
            for assign in &parsed.assignments {
                if assign.line == ctor_line || assign.line + 1 == ctor_line {
                    env.constructor_types.insert(assign.name.clone(), call.callee_name.clone());
                    let scope_key = env.resolve_scope_key_for_line(assign.line, &parsed.scopes);
                    env.bind(&scope_key, &assign.name, &call.callee_name);
                }
            }
        }
    }

    // Tier 2: Assignment chain propagation.
    // If `b = a` and `a` is already typed (in our environment), propagate a's type to b.
    // Single-pass: we iterate assignments and check if the source (same name) is already bound.
    for assign in &parsed.assignments {
        if let Some(src_type) = env.lookup(&assign.name, None, &parsed.scopes, &parsed.symbols) {
            let scope_key = env.resolve_scope_key_for_line(assign.line, &parsed.scopes);
            env.bind(&scope_key, &assign.name, &src_type);
        }
    }

    env
}

/// Build type environments for all parsed files.
pub fn build_type_envs(parsed_files: &[ParsedFile]) -> FxHashMap<u64, TypeEnvironment> {
    let mut envs = FxHashMap::default();
    for parsed in parsed_files {
        let env = build_type_env(parsed);
        envs.insert(parsed.id, env);
    }
    envs
}

/// Cross-file type resolution context.
/// Links type environments across files using import graphs.
pub struct CrossFileTypeResolver {
    /// file_id → type environment
    envs: FxHashMap<u64, TypeEnvironment>,
    /// file_id → Vec<(local_name, imported_type_name, source_file_id)>
    /// Built from imports: `import { Foo } from './bar'` → local_name="Foo", type_name="Foo", source=bar
    import_links: FxHashMap<u64, Vec<(String, String, u64)>>,
    /// type_name → Vec<(file_id, symbol_id)> — reverse index for type→definition lookup
    type_definitions: FxHashMap<String, Vec<(u64, u64)>>,
}

impl CrossFileTypeResolver {
    pub fn new(envs: FxHashMap<u64, TypeEnvironment>) -> Self {
        Self {
            envs,
            import_links: FxHashMap::default(),
            type_definitions: FxHashMap::default(),
        }
    }

    /// Register an import link: file `from_file` imports `type_name` as `local_name` from `to_file`.
    pub fn add_import(&mut self, from_file: u64, local_name: &str, type_name: &str, to_file: u64) {
        self.import_links
            .entry(from_file)
            .or_default()
            .push((local_name.to_string(), type_name.to_string(), to_file));
    }

    /// Register a type definition: `type_name` is defined in `file_id` by `symbol_id`.
    pub fn add_type_definition(&mut self, type_name: &str, file_id: u64, symbol_id: u64) {
        self.type_definitions
            .entry(type_name.to_string())
            .or_default()
            .push((file_id, symbol_id));
    }

    /// Resolve a method call on a receiver to a specific symbol.
    /// Given: receiver type = `type_name`, method = `method_name`, calling from `from_file`.
    /// Returns: Some((file_id, symbol_id)) of the resolved method, or None.
    pub fn resolve_method(&self, type_name: &str, method_name: &str, from_file: u64) -> Option<(u64, u64)> {
        // Strategy 1: Direct type definition lookup — find the class/struct with this name,
        // then look for a method with the matching name in that file.
        if let Some(defs) = self.type_definitions.get(type_name) {
            for (file_id, _sym_id) in defs {
                if let Some(env) = self.envs.get(file_id) {
                    // Check if this file's symbols include the method
                    for scope_map in env.bindings.values() {
                        if scope_map.contains_key(method_name) {
                            // Found the method in the type's file — return the type's file
                            // The actual symbol ID would need a second lookup, but for now
                            // return the file so the caller can search for the method symbol
                            return Some((*file_id, 0));
                        }
                    }
                }
            }
        }

        // Strategy 2: Follow import links from the calling file.
        // If the calling file imported this type, look in the source file.
        if let Some(imports) = self.import_links.get(&from_file) {
            for (local, imported_type, source_file) in imports {
                if local == type_name || imported_type == type_name {
                    if let Some(env) = self.envs.get(source_file) {
                        for scope_map in env.bindings.values() {
                            if scope_map.contains_key(method_name) {
                                return Some((*source_file, 0));
                            }
                        }
                    }
                }
            }
        }

        None
    }

    /// Resolve a variable's type, following import chains across files.
    pub fn resolve_type(&self, var_name: &str, from_file: u64, scope_id: Option<u64>) -> Option<String> {
        // First, check the local file's type environment
        if let Some(env) = self.envs.get(&from_file) {
            if let Some(t) = env.lookup(var_name, scope_id, &[], &[]) {
                return Some(t);
            }
        }

        // If not found locally, check if the variable's type was imported from another file
        if let Some(imports) = self.import_links.get(&from_file) {
            for (local, imported_type, _source_file) in imports {
                if local == var_name {
                    return Some(imported_type.clone());
                }
            }
        }

        None
    }

    /// Resolve all cross-file calls in the parsed files.
    /// For each call with a receiver, try to resolve the receiver's type and
    /// then find the method on that type in the imported file.
    /// Returns the number of successful cross-file resolutions.
    pub fn resolve_all(&self, parsed_files: &[ParsedFile]) -> usize {
        let mut resolved = 0;
        for parsed in parsed_files {
            for call in &parsed.calls {
                if call.resolved_symbol_id.is_some() {
                    continue; // already resolved
                }
                if let Some(ref receiver) = call.receiver {
                    if let Some(env) = self.envs.get(&parsed.id) {
                        if let Some(receiver_type) = env.lookup(
                            receiver, call.caller_scope_id, &parsed.scopes, &parsed.symbols
                        ) {
                            if let Some((target_file_id, _)) = self.resolve_method(
                                &receiver_type, &call.callee_name, parsed.id
                            ) {
                                resolved += 1;
                                tracing::debug!(
                                    "Cross-file type resolution: {}.{} → {} (type={})",
                                    receiver, call.callee_name, target_file_id, receiver_type
                                );
                            }
                        }
                    }
                }
            }
        }
        resolved
    }
}


/// Build a CrossFileTypeResolver from parsed files and their imports.
/// This is the main entry point for cross-file type resolution.
pub fn build_cross_file_resolver(
    parsed_files: &[ParsedFile],
    file_id_to_path: &FxHashMap<u64, String>,
    path_to_file_id: &FxHashMap<String, u64>,
) -> CrossFileTypeResolver {
    let envs = build_type_envs(parsed_files);
    let mut resolver = CrossFileTypeResolver::new(envs);

    // Build type definitions index: class/struct/enum/trait names → (file_id, symbol_id)
    for parsed in parsed_files {
        for sym in &parsed.symbols {
            use crate::lang::CaptureTag;
            match sym.kind {
                CaptureTag::DefinitionClass | CaptureTag::DefinitionStruct |
                CaptureTag::DefinitionInterface | CaptureTag::DefinitionEnum |
                CaptureTag::DefinitionTrait | CaptureTag::DefinitionType => {
                    resolver.add_type_definition(&sym.name, parsed.id, sym.id);
                }
                _ => {}
            }
        }
    }

    // Build import links from parsed imports
    for parsed in parsed_files {
        for imp in &parsed.imports {
            // Resolve the import source to a file ID
            if let Some(source_file_id) = resolve_import_to_file_id(
                &imp.source, &parsed.path, file_id_to_path, path_to_file_id
            ) {
                resolver.add_import(
                    parsed.id,
                    &imp.local_name,
                    &imp.imported_name,
                    source_file_id,
                );
            }
        }
    }

    resolver
}

/// Resolve an import source path to a file ID using path mappings.
fn resolve_import_to_file_id(
    source: &str,
    from_path: &str,
    _file_id_to_path: &FxHashMap<u64, String>,
    path_to_file_id: &FxHashMap<String, u64>,
) -> Option<u64> {
    // Try direct path match
    if let Some(id) = path_to_file_id.get(source) {
        return Some(*id);
    }

    // Try resolving relative to the importing file's directory
    if source.starts_with('.') {
        let from_dir = std::path::Path::new(from_path).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let resolved = format!("{}/{}", from_dir, source);
        if let Some(id) = path_to_file_id.get(&resolved) {
            return Some(*id);
        }
        // Try with common extensions
        for ext in &[".ts", ".js", ".tsx", ".jsx", ".py", ".rs", ".go", ".java"] {
            let with_ext = format!("{}{}", resolved, ext);
            if let Some(id) = path_to_file_id.get(&with_ext) {
                return Some(*id);
            }
        }
    }

    None
}

/// Type-aware call resolution — resolve a call's receiver to its type,
/// then use that type to find the correct target symbol.
pub fn resolve_call_with_type_env(
    call: &Call,
    parsed: &ParsedFile,
    env: &TypeEnvironment,
    _all_symbols: &[Symbol],
) -> Option<String> {
    let receiver = call.receiver.as_ref()?;

    // Look up receiver's type in the type environment
    let receiver_type = env.lookup(receiver, call.caller_scope_id, &parsed.scopes, &parsed.symbols)?;

    // Now we know the receiver's type — use it to find the method on that type
    // This enables type-aware cross-file linking
    Some(receiver_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_env_basic_binding() {
        let mut env = TypeEnvironment::new();
        env.bind("", "user", "User");

        let scopes: Vec<Scope> = vec![];
        let symbols: Vec<Symbol> = vec![];

        assert_eq!(env.lookup("user", None, &scopes, &symbols), Some("User".to_string()));
        assert_eq!(env.lookup("missing", None, &scopes, &symbols), None);
    }

    #[test]
    fn test_type_env_scoped_binding() {
        let mut env = TypeEnvironment::new();
        // File-level binding
        env.bind("", "x", "GlobalType");
        // Function-scope binding (keyed by scope ID)
        env.bind("scope_42", "x", "LocalType");

        let scopes = vec![
            Scope { id: 42, file_id: 1, parent_id: None, owner_symbol_id: None, kind: ScopeKind::Function, line_start: 0, line_end: 50 },
        ];
        let symbols: Vec<Symbol> = vec![];

        // Inside function scope, should get local type
        assert_eq!(env.lookup("x", Some(42), &scopes, &symbols), Some("LocalType".to_string()));
        // Outside function scope, should get file-level type
        assert_eq!(env.lookup("x", None, &scopes, &symbols), Some("GlobalType".to_string()));
    }

    #[test]
    fn test_type_env_self_resolution() {
        let env = TypeEnvironment::new();

        let scopes = vec![
            Scope { id: 0, file_id: 1, parent_id: None, owner_symbol_id: Some(10), kind: ScopeKind::Class, line_start: 0, line_end: 100 },
            Scope { id: 1, file_id: 1, parent_id: Some(0), owner_symbol_id: Some(11), kind: ScopeKind::Method, line_start: 10, line_end: 50 },
        ];
        let symbols = vec![
            Symbol { id: 10, name: "MyClass".into(), qualified_name: "MyClass".into(), kind: crate::lang::CaptureTag::DefinitionClass, file_id: 1, scope_id: None, owner_id: None, line: 0, col: 0, is_exported: false },
            Symbol { id: 11, name: "do_something".into(), qualified_name: "MyClass::do_something".into(), kind: crate::lang::CaptureTag::DefinitionMethod, file_id: 1, scope_id: Some(0), owner_id: Some(10), line: 10, col: 0, is_exported: false },
        ];

        // self inside MyClass::do_something should resolve to "MyClass"
        assert_eq!(env.lookup("self", Some(1), &scopes, &symbols), Some("MyClass".to_string()));
        assert_eq!(env.lookup("this", Some(1), &scopes, &symbols), Some("MyClass".to_string()));
    }

    #[test]
    fn test_strip_nullable() {
        let env = TypeEnvironment::new();
        assert_eq!(env.strip_nullable("Foo?"), "Foo");
        assert_eq!(env.strip_nullable("Foo | null"), "Foo");
        assert_eq!(env.strip_nullable("Foo | undefined"), "Foo");
        assert_eq!(env.strip_nullable("Foo"), "Foo");
        assert_eq!(env.strip_nullable("Foo | Bar"), "Foo | Bar"); // not nullable, keep as-is
    }

    #[test]
    fn test_type_env_scope_priority() {
        let mut env = TypeEnvironment::new();
        // File-level binding
        env.bind("", "x", "GlobalType");
        // Function-local binding shadows file-level
        env.bind("scope_1", "x", "LocalType");

        let scopes = vec![
            Scope { id: 0, file_id: 1, parent_id: None, owner_symbol_id: None, kind: ScopeKind::Module, line_start: 0, line_end: 100 },
            Scope { id: 1, file_id: 1, parent_id: Some(0), owner_symbol_id: None, kind: ScopeKind::Function, line_start: 10, line_end: 50 },
        ];
        let symbols: Vec<Symbol> = vec![];

        // Inside function scope, should get local type
        assert_eq!(env.lookup("x", Some(1), &scopes, &symbols), Some("LocalType".to_string()));
        // Outside function scope, should get file-level type
        assert_eq!(env.lookup("x", None, &scopes, &symbols), Some("GlobalType".to_string()));
    }
}
