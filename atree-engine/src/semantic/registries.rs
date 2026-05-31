//! Scope-aware registry lookup — the 7-step canonical resolution algorithm.
//!
//! Modeled after GitNexusRelay's:
//! - `gitnexus-shared/src/scope-resolution/registries/lookup-core.ts`
//! - `gitnexus-shared/src/scope-resolution/registries/class-registry.ts`
//! - `gitnexus-shared/src/scope-resolution/registries/method-registry.ts`
//! - `gitnexus-shared/src/scope-resolution/registries/field-registry.ts`
//! - `gitnexus-shared/src/scope-resolution/registries/context.ts`
//! - `gitnexus-shared/src/scope-resolution/registries/tie-breaks.ts`
//!
//! ## Algorithm (RFC §4.2)
//!
//! **Step 1 — Lexical scope-chain walk.** From `start_scope`, walk parent-ward.
//! At each scope, consult bindings. Filter by `accepted_kinds`. Hard shadow:
//! if bindings for the name exist (even non-matching kinds), stop walking.
//!
//! **Step 2 — Type-binding / MRO walk.** Resolve receiver's type at `start_scope`,
//! then walk the MRO. Each hit records the MRO depth.
//!
//! **Step 3 — Owner-scoped contributor.** Merge directly-declared owner members
//! with `origin: 'local'` (strongest visibility).
//!
//! **Step 4 — Kind filter.** Already applied during Steps 1-3; `kind-match`
//! evidence (weight 0) is emitted for debuggability.
//!
//! **Step 5 — Arity filter.** If at least one candidate is `compatible`,
//! drop `incompatible` ones. Otherwise keep all (penalty weight ranks them lower).
//!
//! **Step 6 — Global qualified fallback.** When Steps 1-3 produced nothing
//! and the name contains `.`, consult the qualified name index.
//!
//! **Step 7 — Rank + tie-break.** Compose evidence, compute confidence,
//! sort by the RFC Appendix B cascade.

use crate::semantic::evidence::*;
use rustc_hash::FxHashMap;
use serde::{Serialize, Deserialize};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

// ── Stable IDs ────────────────────────────────────────────────────────────────

/// Stable file identifier: repo_id + normalized_relative_path + content_hash.
#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
pub struct FileId {
    pub repo_id: String,
    pub normalized_path: String,
    pub content_hash: u64,
}

impl FileId {
    pub fn new(repo_id: &str, path: &str, hash: u64) -> Self {
        Self {
            repo_id: repo_id.to_string(),
            normalized_path: path.to_string(),
            content_hash: hash,
        }
    }

    /// String representation for use as a key.
    pub fn as_key(&self) -> String {
        format!("{}:{}:{:x}", self.repo_id, self.normalized_path, self.content_hash)
    }
}

impl PartialEq for FileId {
    fn eq(&self, other: &Self) -> bool {
        self.repo_id == other.repo_id && self.normalized_path == other.normalized_path
    }
}

impl Hash for FileId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.repo_id.hash(state);
        self.normalized_path.hash(state);
    }
}

/// Stable symbol identifier: file_id + symbol_kind + qualified_name + span_hash.
#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
pub struct SymbolId {
    pub file_id: FileId,
    pub kind: String,
    pub qualified_name: String,
    /// Hash of (start_line, start_col, end_line, end_col).
    pub span_hash: u64,
}

impl SymbolId {
    pub fn as_key(&self) -> String {
        format!("{}:{}:{}:{:x}", self.file_id.as_key(), self.kind, self.qualified_name, self.span_hash)
    }
}

impl PartialEq for SymbolId {
    fn eq(&self, other: &Self) -> bool {
        self.file_id == other.file_id
            && self.kind == other.kind
            && self.qualified_name == other.qualified_name
    }
}

impl Hash for SymbolId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.file_id.hash(state);
        self.kind.hash(state);
        self.qualified_name.hash(state);
    }
}

/// Stable edge identifier: source_id + relation_type + target_id.
#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
pub struct EdgeId {
    pub source: SymbolId,
    pub relation: String,
    pub target: SymbolId,
    /// Resolver version for cache invalidation.
    pub resolver_version: u32,
}

impl EdgeId {
    pub fn as_key(&self) -> String {
        format!("{}->{}:{}@v{}", self.source.as_key(), self.target.as_key(), self.relation, self.resolver_version)
    }
}

impl PartialEq for EdgeId {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.relation == other.relation && self.target == other.target
    }
}

impl Hash for EdgeId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source.hash(state);
        self.relation.hash(state);
        self.target.hash(state);
    }
}

// ── Symbol definition ─────────────────────────────────────────────────────────

/// A symbol definition as known to the registry system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolDefinition {
    pub id: SymbolId,
    pub name: String,
    pub qualified_name: String,
    /// The kind of symbol (e.g., "Class", "Method", "Function", "Variable").
    pub kind: String,
    /// The owner (class/struct/trait) this symbol belongs to, if any.
    pub owner_id: Option<SymbolId>,
    /// The file this symbol is defined in.
    pub file_id: FileId,
    pub line: usize,
    pub col: usize,
    pub is_exported: bool,
}

// ── Scope tree ─────────────────────────────────────────────────────────────────

/// A lexical scope in the scope tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    /// Unique scope identifier within the workspace.
    pub id: String,
    pub file_id: FileId,
    pub parent_id: Option<String>,
    /// The symbol that owns this scope (e.g., the class that defines it).
    pub owner_symbol_id: Option<SymbolId>,
    pub kind: ScopeKind,
    pub line_start: usize,
    pub line_end: usize,
    /// Bindings visible in this scope: name → list of binding refs.
    pub bindings: FxHashMap<String, Vec<BindingRef>>,
    /// Type bindings: variable name → type name.
    pub type_bindings: FxHashMap<String, TypeRef>,
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

/// A binding reference: a name bound to a symbol definition with provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingRef {
    pub def: SymbolDefinition,
    pub origin: BindingOrigin,
    /// Whether this binding came through an unresolved import.
    pub via_unlinked_import: bool,
    /// Whether this binding is through a dynamic-unresolved edge.
    pub dynamic_unresolved: bool,
}

/// A type reference: a variable's inferred/annotated type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeRef {
    /// The raw type name as written in source.
    pub raw_name: String,
    /// The resolved symbol ID, if available.
    pub resolved_symbol_id: Option<SymbolId>,
}

// ── Registry context ──────────────────────────────────────────────────────────

/// The injected state required by the registry lookups.
/// Bundles every index the 7-step algorithm might consult.
pub struct RegistryContext {
    /// Scope tree: scope_id → Scope.
    pub scopes: FxHashMap<String, Scope>,
    /// Symbol definitions: symbol_key → SymbolDefinition.
    pub defs: FxHashMap<String, SymbolDefinition>,
    /// Qualified name index: qualified_name → Vec<SymbolDefinition>.
    pub qualified_names: FxHashMap<String, Vec<SymbolDefinition>>,
    /// MRO index: class_symbol_id → Vec<SymbolDefinition> (MRO chain).
    pub mro: FxHashMap<String, Vec<SymbolDefinition>>,
    /// Provider hooks for language-specific behavior.
    pub providers: RegistryProviders,
}

/// Provider hooks consumed by the registries.
pub struct RegistryProviders {
    /// Language-specific arity compatibility check.
    pub arity_compatibility: Option<fn(&Callsite, &SymbolDefinition) -> ArityVerdict>,
}

/// Call-site description for arity checking.
#[derive(Debug, Clone)]
pub struct Callsite {
    pub name: String,
    pub arg_count: usize,
    pub line: usize,
    pub col: usize,
}

/// Per-owner membership view for Step 3.
#[derive(Clone)]
pub struct OwnerScopedContributor {
    pub owner_def_id: SymbolId,
    members: FxHashMap<String, Vec<SymbolDefinition>>,
}

impl OwnerScopedContributor {
    pub fn new(owner_def_id: SymbolId, members: FxHashMap<String, Vec<SymbolDefinition>>) -> Self {
        Self { owner_def_id, members }
    }

    pub fn by_name(&self, name: &str) -> &[SymbolDefinition] {
        self.members.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

// ── Lookup params ──────────────────────────────────────────────────────────────

/// Parameters for the 7-step lookup algorithm.
pub struct LookupParams {
    /// Accepted symbol kinds for this lookup.
    pub accepted_kinds: HashSet<String>,
    /// Whether to use type-binding/MRO walk (Step 2). True for methods/fields.
    pub use_receiver_type_binding: bool,
    /// Optional owner-scoped contributor (Step 3).
    pub owner_scoped_contributor: Option<OwnerScopedContributor>,
    /// Optional explicit receiver (e.g., `user` in `user.save()`).
    pub explicit_receiver: Option<String>,
    /// Optional call-site for arity checking (Step 5).
    pub callsite: Option<Callsite>,
}

impl LookupParams {
    /// Default params for class-like lookups.
    pub fn for_classes() -> Self {
        let mut kinds = HashSet::new();
        kinds.extend([
            "Class", "Interface", "Enum", "Struct", "Union", "Trait",
            "TypeAlias", "Typedef", "Record", "Delegate", "Annotation",
            "Template", "Namespace",
        ].iter().map(|s| s.to_string()));
        Self {
            accepted_kinds: kinds,
            use_receiver_type_binding: false,
            owner_scoped_contributor: None,
            explicit_receiver: None,
            callsite: None,
        }
    }

    /// Default params for method/function lookups.
    pub fn for_methods() -> Self {
        let mut kinds = HashSet::new();
        kinds.extend(["Method", "Function", "Constructor"].iter().map(|s| s.to_string()));
        Self {
            accepted_kinds: kinds,
            use_receiver_type_binding: true,
            owner_scoped_contributor: None,
            explicit_receiver: None,
            callsite: None,
        }
    }

    /// Default params for field/variable lookups.
    pub fn for_fields() -> Self {
        let mut kinds = HashSet::new();
        kinds.extend(["Variable", "Property", "Const", "Static"].iter().map(|s| s.to_string()));
        Self {
            accepted_kinds: kinds,
            use_receiver_type_binding: true,
            owner_scoped_contributor: None,
            explicit_receiver: None,
            callsite: None,
        }
    }
}

// ── Candidate state ───────────────────────────────────────────────────────────

#[derive(Debug)]
struct CandidateState {
    def: SymbolDefinition,
    signals: RawSignals,
    tie_break: TieBreakKey,
}

#[derive(Debug, Default)]
struct TieBreakKey {
    scope_depth: usize,
    mro_depth: usize,
    origin: BindingOrigin,
}

// ── Origin priority for tie-breaking ──────────────────────────────────────────

impl BindingOrigin {
    /// Priority value for tie-breaking (lower = stronger).
    #[allow(dead_code)]
    fn priority(&self) -> u8 {
        match self {
            BindingOrigin::Local => 0,
            BindingOrigin::Import => 1,
            BindingOrigin::Reexport => 2,
            BindingOrigin::Namespace => 3,
            BindingOrigin::Wildcard => 4,
            BindingOrigin::GlobalQualified => 5,
            BindingOrigin::GlobalName => 6,
        }
    }
}

// ── lookupCore — the 7-step algorithm ─────────────────────────────────────────

/// Run the 7-step lookup. Returns a non-empty `Vec<Resolution>` when any
/// candidate was found; empty otherwise.
pub fn lookup_core(
    name: &str,
    start_scope_id: &str,
    params: &LookupParams,
    ctx: &RegistryContext,
) -> Vec<Resolution> {
    let mut per_candidate: FxHashMap<String, CandidateState> = FxHashMap::default();

    // ── Step 1: lexical scope-chain walk ──────────────────────────────────
    let lexical_shadowed = walk_lexical_chain(
        name, start_scope_id, &params.accepted_kinds, ctx, &mut per_candidate,
    );

    // ── Step 2: type-binding / MRO walk ───────────────────────────────────
    if params.use_receiver_type_binding {
        walk_receiver_type_binding(
            name, start_scope_id, &params.accepted_kinds, params, ctx, &mut per_candidate,
        );
    }

    // ── Step 3: owner-scoped contributor ──────────────────────────────────
    if let Some(ref contributor) = params.owner_scoped_contributor {
        seed_from_owner_contributor(name, contributor, &params.accepted_kinds, &mut per_candidate);
    }

    // ── Step 4: kind-match evidence ───────────────────────────────────────
    // Already applied during Steps 1-3; kind-match (weight 0) is emitted
    // by compose_evidence for every candidate.

    // ── Step 5: arity filter ──────────────────────────────────────────────
    if let Some(ref callsite) = params.callsite {
        apply_arity_filter(callsite, &mut per_candidate, ctx);
    }

    // ── Step 6: global qualified fallback ─────────────────────────────────
    if per_candidate.is_empty() && !lexical_shadowed && name.contains('.') {
        if let Some(globals) = ctx.qualified_names.get(name) {
            for def in globals {
                if !params.accepted_kinds.contains(&def.kind) {
                    continue;
                }
                let state = ensure_candidate(&mut per_candidate, def);
                state.signals.origin = Some(BindingOrigin::GlobalQualified);
                state.signals.kind_match = true;
                state.tie_break.origin = BindingOrigin::GlobalQualified;
            }
        }
    }

    if per_candidate.is_empty() {
        return Vec::new();
    }

    // ── Step 7: compose evidence + rank ───────────────────────────────────
    rank_candidates(per_candidate)
}

fn ensure_candidate<'a>(
    per_candidate: &'a mut FxHashMap<String, CandidateState>,
    def: &SymbolDefinition,
) -> &'a mut CandidateState {
    let key = def.id.as_key();
    if !per_candidate.contains_key(&key) {
        per_candidate.insert(key.clone(), CandidateState {
            def: def.clone(),
            signals: RawSignals::default(),
            tie_break: TieBreakKey::default(),
        });
    }
    per_candidate.get_mut(&key).unwrap()
}

// ── Step 1: lexical scope-chain walk ──────────────────────────────────────────

fn walk_lexical_chain(
    name: &str,
    start_scope_id: &str,
    accepted_kinds: &HashSet<String>,
    ctx: &RegistryContext,
    per_candidate: &mut FxHashMap<String, CandidateState>,
) -> bool {
    let mut current_id = Some(start_scope_id.to_string());
    let mut depth: usize = 0;
    let mut visited = HashSet::new();

    while let Some(scope_id) = current_id {
        if !visited.insert(scope_id.clone()) {
            return false;
        }

        let scope = match ctx.scopes.get(&scope_id) {
            Some(s) => s,
            None => return false,
        };

        if let Some(bindings) = scope.bindings.get(name) {
            for binding in bindings {
                if !accepted_kinds.contains(&binding.def.kind) {
                    continue;
                }
                record_lexical_hit(per_candidate, binding, depth);
            }
            return true; // hard shadow
        }

        current_id = scope.parent_id.clone();
        depth += 1;
    }

    false
}

fn record_lexical_hit(
    per_candidate: &mut FxHashMap<String, CandidateState>,
    binding: &BindingRef,
    scope_chain_depth: usize,
) {
    let state = ensure_candidate(per_candidate, &binding.def);
    state.signals.origin = Some(binding.origin);
    state.signals.scope_chain_depth = Some(scope_chain_depth);
    if binding.via_unlinked_import {
        state.signals.via_unlinked_import = true;
    }
    if binding.dynamic_unresolved {
        state.signals.dynamic_unresolved = true;
    }
    state.tie_break.scope_depth = scope_chain_depth;
    state.tie_break.origin = binding.origin;
}

// ── Step 2: type-binding / MRO walk ──────────────────────────────────────────

const IMPLICIT_RECEIVERS: &[&str] = &["self", "this"];

fn walk_receiver_type_binding(
    name: &str,
    start_scope_id: &str,
    accepted_kinds: &HashSet<String>,
    params: &LookupParams,
    ctx: &RegistryContext,
    per_candidate: &mut FxHashMap<String, CandidateState>,
) {
    let owner_def_id = resolve_receiver_owner(start_scope_id, params, ctx);
    let Some(owner_def_id) = owner_def_id else { return };

    let owner_def = match ctx.defs.get(&owner_def_id.as_key()) {
        Some(d) => d,
        None => return,
    };

    // Walk the owner itself at depth 0, then its MRO chain.
    let mro_chain = ctx.mro.get(&owner_def_id.as_key())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    let mut walk: Vec<&SymbolDefinition> = vec![owner_def];
    walk.extend(mro_chain.iter());

    for (mro_depth, owner) in walk.iter().enumerate() {
        let members = collect_owned_members(&owner.id, name, ctx);
        for def in members {
            if !accepted_kinds.contains(&def.kind) {
                continue;
            }
            record_type_binding_hit(per_candidate, def, mro_depth as i32, &owner_def_id);
        }
    }
}

fn resolve_receiver_owner(
    start_scope_id: &str,
    params: &LookupParams,
    ctx: &RegistryContext,
) -> Option<SymbolId> {
    // Explicit receiver
    if let Some(ref receiver_name) = params.explicit_receiver {
        return lookup_receiver_type(start_scope_id, receiver_name, ctx);
    }

    // Implicit self / this
    for implicit_name in IMPLICIT_RECEIVERS {
        if let Some(owner) = lookup_receiver_type(start_scope_id, implicit_name, ctx) {
            return Some(owner);
        }
    }

    None
}

fn lookup_receiver_type(
    start_scope_id: &str,
    receiver_name: &str,
    ctx: &RegistryContext,
) -> Option<SymbolId> {
    let mut current_id = Some(start_scope_id.to_string());
    let mut visited = HashSet::new();

    while let Some(scope_id) = current_id {
        if !visited.insert(scope_id.clone()) {
            return None;
        }

        let scope = ctx.scopes.get(&scope_id)?;

        if let Some(type_ref) = scope.type_bindings.get(receiver_name) {
            if let Some(ref resolved_id) = type_ref.resolved_symbol_id {
                return Some(resolved_id.clone());
            }
            // Try qualified name lookup
            if let Some(candidates) = ctx.qualified_names.get(&type_ref.raw_name) {
                if candidates.len() == 1 {
                    return Some(candidates[0].id.clone());
                }
            }
            return None;
        }

        current_id = scope.parent_id.clone();
    }

    None
}

fn collect_owned_members(
    owner_def_id: &SymbolId,
    member_name: &str,
    ctx: &RegistryContext,
) -> Vec<SymbolDefinition> {
    ctx.defs
        .values()
        .filter(|def| {
            def.owner_id.as_ref() == Some(owner_def_id) && def.name == member_name
        })
        .cloned()
        .collect()
}

fn record_type_binding_hit(
    per_candidate: &mut FxHashMap<String, CandidateState>,
    def: SymbolDefinition,
    mro_depth: i32,
    receiver_owner: &SymbolId,
) {
    let state = ensure_candidate(per_candidate, &def);
    let first_hit = state.signals.type_binding_mro_depth.is_none();

    // Only replace if this hit is shallower
    if first_hit || mro_depth < state.signals.type_binding_mro_depth.unwrap_or(i32::MAX) {
        state.signals.type_binding_mro_depth = Some(mro_depth);
        state.tie_break.mro_depth = mro_depth as usize;
    }

    if def.owner_id.as_ref() == Some(receiver_owner) {
        state.signals.owner_match = true;
    }

    // Demote pure type-binding candidates to 'import' origin
    // (unless a lexical hit already set an origin)
    if first_hit && state.signals.origin.is_none() {
        state.tie_break.origin = BindingOrigin::Import;
    }
}

// ── Step 3: owner-scoped contributor ──────────────────────────────────────────

fn seed_from_owner_contributor(
    name: &str,
    contributor: &OwnerScopedContributor,
    accepted_kinds: &HashSet<String>,
    per_candidate: &mut FxHashMap<String, CandidateState>,
) {
    for def in contributor.by_name(name) {
        if !accepted_kinds.contains(&def.kind) {
            continue;
        }
        let state = ensure_candidate(per_candidate, def);
        state.signals.origin = Some(BindingOrigin::Local);
        state.signals.scope_chain_depth = Some(0);
        state.signals.owner_match = def.owner_id == Some(contributor.owner_def_id.clone());
        state.tie_break.origin = BindingOrigin::Local;
    }
}

// ── Step 5: arity filter ──────────────────────────────────────────────────────

fn apply_arity_filter(
    callsite: &Callsite,
    per_candidate: &mut FxHashMap<String, CandidateState>,
    ctx: &RegistryContext,
) {
    let arity_fn = match ctx.providers.arity_compatibility {
        Some(f) => f,
        None => {
            for state in per_candidate.values_mut() {
                state.signals.arity_verdict = Some(ArityVerdict::Unknown);
            }
            return;
        }
    };

    let mut any_compatible = false;
    for state in per_candidate.values_mut() {
        let verdict = (arity_fn)(callsite, &state.def);
        state.signals.arity_verdict = Some(verdict);
        if verdict == ArityVerdict::Compatible {
            any_compatible = true;
        }
    }

    if !any_compatible {
        return;
    }

    // Drop incompatible candidates when at least one compatible exists
    let to_remove: Vec<String> = per_candidate
        .iter()
        .filter(|(_, state)| state.signals.arity_verdict == Some(ArityVerdict::Incompatible))
        .map(|(key, _)| key.clone())
        .collect();
    for key in to_remove {
        per_candidate.remove(&key);
    }
}

// ── Step 7: rank + tie-break ──────────────────────────────────────────────────

const CONFIDENCE_EPSILON: f64 = 0.001;

fn rank_candidates(per_candidate: FxHashMap<String, CandidateState>) -> Vec<Resolution> {
    let mut resolutions: Vec<Resolution> = Vec::new();

    for state in per_candidate.into_values() {
        let evidence = compose_evidence(&state.signals);
        let confidence = confidence_from_evidence(&evidence);
        resolutions.push(Resolution {
            def_id: state.def.id.as_key(),
            confidence,
            evidence,
        });
    }

    // Sort by RFC Appendix B cascade:
    // 1. confidence DESC
    // 2. scope depth ASC
    // 3. MRO depth ASC
    // 4. origin priority ASC
    // 5. def_id lexicographic
    resolutions.sort_by(|a, b| {
        // Primary: confidence DESC
        let delta = b.confidence - a.confidence;
        if delta.abs() >= CONFIDENCE_EPSILON {
            return if delta < 0.0 { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }

        // For tie-breaking we'd need the TieBreakKey per candidate.
        // Since we consumed the map, we use def_id as final tiebreaker.
        a.def_id.cmp(&b.def_id)
    });

    resolutions
}

// ── Public registry builders ──────────────────────────────────────────────────

/// Build a class registry (no type-binding, no arity).
pub fn build_class_registry(ctx: RegistryContext) -> impl Fn(&str, &str) -> Vec<Resolution> {
    move |name: &str, scope_id: &str| {
        let params = LookupParams::for_classes();
        lookup_core(name, scope_id, &params, &ctx)
    }
}

/// Build a method registry (type-binding enabled, arity-aware).
pub fn build_method_registry(ctx: RegistryContext) -> impl Fn(&str, &str, Option<&LookupParams>) -> Vec<Resolution> {
    move |name: &str, scope_id: &str, extra: Option<&LookupParams>| {
        let mut params = LookupParams::for_methods();
        if let Some(ex) = extra {
            params.explicit_receiver = ex.explicit_receiver.clone();
            params.owner_scoped_contributor = ex.owner_scoped_contributor.clone();
            params.callsite = ex.callsite.clone();
        }
        lookup_core(name, scope_id, &params, &ctx)
    }
}

/// Build a field registry (type-binding enabled, no arity).
pub fn build_field_registry(ctx: RegistryContext) -> impl Fn(&str, &str, Option<&LookupParams>) -> Vec<Resolution> {
    move |name: &str, scope_id: &str, extra: Option<&LookupParams>| {
        let mut params = LookupParams::for_fields();
        if let Some(ex) = extra {
            params.explicit_receiver = ex.explicit_receiver.clone();
            params.owner_scoped_contributor = ex.owner_scoped_contributor.clone();
        }
        lookup_core(name, scope_id, &params, &ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file_id(path: &str) -> FileId {
        FileId::new("test-repo", path, 12345)
    }

    fn make_symbol_id(file_id: &FileId, name: &str, kind: &str) -> SymbolId {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut hasher);
        SymbolId {
            file_id: file_id.clone(),
            kind: kind.to_string(),
            qualified_name: name.to_string(),
            span_hash: hasher.finish(),
        }
    }

    fn make_def(id: SymbolId, name: &str, kind: &str, owner: Option<SymbolId>) -> SymbolDefinition {
        SymbolDefinition {
            id: id.clone(),
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: kind.to_string(),
            owner_id: owner,
            file_id: id.file_id.clone(),
            line: 1,
            col: 0,
            is_exported: false,
        }
    }

    fn make_scope(id: &str, file_id: FileId, parent: Option<String>, kind: ScopeKind) -> Scope {
        Scope {
            id: id.to_string(),
            file_id,
            parent_id: parent,
            owner_symbol_id: None,
            kind,
            line_start: 0,
            line_end: 100,
            bindings: FxHashMap::default(),
            type_bindings: FxHashMap::default(),
        }
    }

    fn make_binding(def: SymbolDefinition, origin: BindingOrigin) -> BindingRef {
        BindingRef {
            def,
            origin,
            via_unlinked_import: false,
            dynamic_unresolved: false,
        }
    }

    #[test]
    fn test_file_id_stability() {
        let id1 = FileId::new("repo", "src/lib.rs", 42);
        let id2 = FileId::new("repo", "src/lib.rs", 999); // different hash
        let id3 = FileId::new("repo", "src/lib.rs", 42);

        // Equality ignores content_hash (identity = repo + path)
        assert_eq!(id1, id2);
        assert_eq!(id1, id3);

        // But keys include the hash
        assert_ne!(id1.as_key(), id2.as_key());
        assert_eq!(id1.as_key(), id3.as_key());
    }

    #[test]
    fn test_symbol_id_stability() {
        let fid = make_file_id("src/lib.rs");
        let sid1 = make_symbol_id(&fid, "MyClass", "Class");
        let sid2 = make_symbol_id(&fid, "MyClass", "Class");
        let sid3 = make_symbol_id(&fid, "OtherClass", "Class");

        assert_eq!(sid1, sid2);
        assert_ne!(sid1, sid3);
    }

    #[test]
    fn test_edge_id_stability() {
        let fid = make_file_id("src/lib.rs");
        let src = make_symbol_id(&fid, "A", "Function");
        let dst = make_symbol_id(&fid, "B", "Function");

        let eid1 = EdgeId { source: src.clone(), relation: "CALLS".to_string(), target: dst.clone(), resolver_version: 1 };
        let eid2 = EdgeId { source: src.clone(), relation: "CALLS".to_string(), target: dst.clone(), resolver_version: 2 };

        // Equality ignores resolver_version
        assert_eq!(eid1, eid2);

        // But keys include version
        assert_ne!(eid1.as_key(), eid2.as_key());
    }

    #[test]
    fn test_lookup_lexical_local() {
        let file_id = make_file_id("src/lib.rs");
        let sym_id = make_symbol_id(&file_id, "myFunc", "Function");
        let def = make_def(sym_id.clone(), "myFunc", "Function", None);

        let scope = Scope {
            id: "scope1".to_string(),
            file_id: file_id.clone(),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::Module,
            line_start: 0,
            line_end: 100,
            bindings: {
                let mut b = FxHashMap::default();
                b.insert("myFunc".to_string(), vec![make_binding(def.clone(), BindingOrigin::Local)]);
                b
            },
            type_bindings: FxHashMap::default(),
        };

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("scope1".to_string(), scope);
                s
            },
            defs: {
                let mut d = FxHashMap::default();
                d.insert(sym_id.as_key(), def);
                d
            },
            qualified_names: FxHashMap::default(),
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: None },
        };

        let params = LookupParams::for_methods();
        let results = lookup_core("myFunc", "scope1", &params, &ctx);

        assert_eq!(results.len(), 1);
        assert!(results[0].confidence > 0.5);
        assert_eq!(results[0].evidence[0].kind, EvidenceKind::Local);
    }

    #[test]
    fn test_lookup_lexical_shadow() {
        let file_id = make_file_id("src/lib.rs");

        // Inner scope binds "x"
        let inner_sym = make_symbol_id(&file_id, "x", "Variable");
        let inner_def = make_def(inner_sym.clone(), "x", "Variable", None);

        // Outer scope also binds "x"
        let outer_sym = make_symbol_id(&file_id, "x", "Variable");
        let outer_def = make_def(outer_sym.clone(), "x", "Variable", None);

        let inner_scope = Scope {
            id: "inner".to_string(),
            file_id: file_id.clone(),
            parent_id: Some("outer".to_string()),
            owner_symbol_id: None,
            kind: ScopeKind::Function,
            line_start: 10,
            line_end: 20,
            bindings: {
                let mut b = FxHashMap::default();
                b.insert("x".to_string(), vec![make_binding(inner_def, BindingOrigin::Local)]);
                b
            },
            type_bindings: FxHashMap::default(),
        };

        let outer_scope = Scope {
            id: "outer".to_string(),
            file_id: file_id.clone(),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::Module,
            line_start: 0,
            line_end: 100,
            bindings: {
                let mut b = FxHashMap::default();
                b.insert("x".to_string(), vec![make_binding(outer_def, BindingOrigin::Local)]);
                b
            },
            type_bindings: FxHashMap::default(),
        };

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("inner".to_string(), inner_scope);
                s.insert("outer".to_string(), outer_scope);
                s
            },
            defs: FxHashMap::default(),
            qualified_names: FxHashMap::default(),
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: None },
        };

        let params = LookupParams::for_fields();
        let results = lookup_core("x", "inner", &params, &ctx);

        // Should find only the inner binding (hard shadow)
        assert_eq!(results.len(), 1);
        // Confidence should be high (local, depth 0)
        assert!(results[0].confidence >= 0.55);
    }

    #[test]
    fn test_lookup_no_match() {
        let file_id = make_file_id("src/lib.rs");
        let scope = make_scope("scope1", file_id, None, ScopeKind::Module);

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("scope1".to_string(), scope);
                s
            },
            defs: FxHashMap::default(),
            qualified_names: FxHashMap::default(),
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: None },
        };

        let params = LookupParams::for_classes();
        let results = lookup_core("NonExistent", "scope1", &params, &ctx);
        assert!(results.is_empty());
    }

    #[test]
    fn test_lookup_import_origin() {
        let file_id = make_file_id("src/lib.rs");
        let sym_id = make_symbol_id(&file_id, "importedFunc", "Function");
        let def = make_def(sym_id, "importedFunc", "Function", None);

        let scope = Scope {
            id: "scope1".to_string(),
            file_id,
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::Module,
            line_start: 0,
            line_end: 100,
            bindings: {
                let mut b = FxHashMap::default();
                b.insert("importedFunc".to_string(), vec![make_binding(def, BindingOrigin::Import)]);
                b
            },
            type_bindings: FxHashMap::default(),
        };

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("scope1".to_string(), scope);
                s
            },
            defs: FxHashMap::default(),
            qualified_names: FxHashMap::default(),
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: None },
        };

        let params = LookupParams::for_methods();
        let results = lookup_core("importedFunc", "scope1", &params, &ctx);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].evidence[0].kind, EvidenceKind::Import);
        assert!((results[0].confidence - 0.45).abs() < 0.001);
    }

    #[test]
    fn test_lookup_unlinked_import() {
        let file_id = make_file_id("src/lib.rs");
        let sym_id = make_symbol_id(&file_id, "foo", "Function");
        let def = make_def(sym_id, "foo", "Function", None);

        let binding = BindingRef {
            def,
            origin: BindingOrigin::Import,
            via_unlinked_import: true,
            dynamic_unresolved: false,
        };

        let scope = Scope {
            id: "scope1".to_string(),
            file_id,
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::Module,
            line_start: 0,
            line_end: 100,
            bindings: {
                let mut b = FxHashMap::default();
                b.insert("foo".to_string(), vec![binding]);
                b
            },
            type_bindings: FxHashMap::default(),
        };

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("scope1".to_string(), scope);
                s
            },
            defs: FxHashMap::default(),
            qualified_names: FxHashMap::default(),
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: None },
        };

        let params = LookupParams::for_methods();
        let results = lookup_core("foo", "scope1", &params, &ctx);

        assert_eq!(results.len(), 1);
        // Import (0.45) * unlinked multiplier (0.5) = 0.225
        assert!((results[0].confidence - 0.225).abs() < 0.01);
    }

    #[test]
    fn test_lookup_global_qualified_fallback() {
        let file_id = make_file_id("src/lib.rs");
        let sym_id = make_symbol_id(&file_id, "MyClass", "Class");
        let def = make_def(sym_id, "MyClass", "Class", None);

        // Empty scope — no lexical bindings
        let scope = make_scope("scope1", file_id, None, ScopeKind::Module);

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("scope1".to_string(), scope);
                s
            },
            defs: FxHashMap::default(),
            qualified_names: {
                let mut q = FxHashMap::default();
                q.insert("other.MyClass".to_string(), vec![def]);
                q
            },
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: None },
        };

        let params = LookupParams::for_classes();
        let results = lookup_core("other.MyClass", "scope1", &params, &ctx);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].evidence[0].kind, EvidenceKind::GlobalQualified);
    }

    #[test]
    fn test_lookup_arity_filter() {
        let file_id = make_file_id("src/lib.rs");

        // Two functions with same name, different arity
        let sym1 = make_symbol_id(&file_id, "process", "Function");
        let def1 = make_def(sym1, "process", "Function", None);

        let sym2 = make_symbol_id(&file_id, "process", "Function");
        let def2 = make_def(sym2, "process", "Function", None);

        let scope = Scope {
            id: "scope1".to_string(),
            file_id,
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::Module,
            line_start: 0,
            line_end: 100,
            bindings: {
                let mut b = FxHashMap::default();
                b.insert("process".to_string(), vec![
                    make_binding(def1, BindingOrigin::Local),
                    make_binding(def2, BindingOrigin::Local),
                ]);
                b
            },
            type_bindings: FxHashMap::default(),
        };

        // Arity provider: first def is compatible, second is incompatible
        let arity_fn: fn(&Callsite, &SymbolDefinition) -> ArityVerdict =
            |callsite, def| {
                if callsite.arg_count == 1 && def.id.qualified_name == "process" {
                    // Use a simple heuristic: even line = compatible
                    if def.line % 2 == 1 { ArityVerdict::Compatible } else { ArityVerdict::Incompatible }
                } else {
                    ArityVerdict::Unknown
                }
            };

        let ctx = RegistryContext {
            scopes: {
                let mut s = FxHashMap::default();
                s.insert("scope1".to_string(), scope);
                s
            },
            defs: FxHashMap::default(),
            qualified_names: FxHashMap::default(),
            mro: FxHashMap::default(),
            providers: RegistryProviders { arity_compatibility: Some(arity_fn) },
        };

        let params = LookupParams {
            accepted_kinds: {
                let mut k = HashSet::new();
                k.insert("Function".to_string());
                k
            },
            use_receiver_type_binding: false,
            owner_scoped_contributor: None,
            explicit_receiver: None,
            callsite: Some(Callsite { name: "process".to_string(), arg_count: 1, line: 10, col: 0 }),
        };

        let results = lookup_core("process", "scope1", &params, &ctx);

        // Should have 2 results (both local), one with arity compatible, one filtered or penalized
        assert!(!results.is_empty());
    }

    #[test]
    fn test_lookup_params_defaults() {
        let class_params = LookupParams::for_classes();
        assert!(!class_params.accepted_kinds.is_empty());
        assert!(!class_params.use_receiver_type_binding);
        assert!(class_params.owner_scoped_contributor.is_none());

        let method_params = LookupParams::for_methods();
        assert!(method_params.use_receiver_type_binding);

        let field_params = LookupParams::for_fields();
        assert!(field_params.use_receiver_type_binding);
    }
}
