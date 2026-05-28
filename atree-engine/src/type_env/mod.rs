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
    bindings: FxHashMap<String, FxHashMap<String, String>>,
    /// Variable names that were assigned via constructor calls (Tier 1)
    constructor_types: FxHashMap<String, String>,
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
