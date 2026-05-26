//! ReferenceIndex — two-way index over reference records.
//!
//! Modeled after GitNexusRelay's `ReferenceIndex` from
//! `gitnexus-shared/src/scope-resolution/types.ts`.
//!
//! Two-way index over `Reference` records, populated during the resolution
//! phase. Scopes stay immutable after finalize; references accumulate here.

use rustc_hash::FxHashMap;
use serde::{Serialize, Deserialize};

// ── Reference types ──────────────────────────────────────────────────────────

/// Stable scope identifier.
pub type ScopeId = String;

/// Stable definition identifier.
pub type DefId = String;

/// Source-text range (1-based lines, 0-based cols).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Range {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

/// A post-resolution usage fact: some code at `at_range` inside `from_scope`
/// references `to_def` with the given confidence/evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// Innermost lexical scope containing `at_range`.
    pub from_scope: ScopeId,
    /// The definition being referenced.
    pub to_def: DefId,
    /// Location of the reference in source.
    pub at_range: Range,
    /// Kind of reference.
    pub kind: ReferenceKind,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f64,
}

/// Kind of reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceKind {
    Call,
    Read,
    Write,
    TypeReference,
    Inherits,
    ImportUse,
}

// ── ReferenceIndex ───────────────────────────────────────────────────────────

/// Two-way index over `Reference` records.
///
/// Maintained during the resolution phase. Scopes stay immutable after
/// finalize; references accumulate here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferenceIndex {
    /// References grouped by source scope.
    by_source_scope: FxHashMap<ScopeId, Vec<Reference>>,
    /// References grouped by target definition.
    by_target_def: FxHashMap<DefId, Vec<Reference>>,
    /// Total reference count.
    total: usize,
}

impl ReferenceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a reference to the index.
    pub fn add(&mut self, reference: Reference) {
        let from = reference.from_scope.clone();
        let to = reference.to_def.clone();
        self.by_source_scope.entry(from).or_default().push(reference.clone());
        self.by_target_def.entry(to).or_default().push(reference);
        self.total += 1;
    }

    /// Add multiple references.
    pub fn add_many(&mut self, references: Vec<Reference>) {
        for r in references {
            self.add(r);
        }
    }

    /// Get all references originating from a given scope.
    pub fn by_source(&self, scope_id: &ScopeId) -> &[Reference] {
        self.by_source_scope.get(scope_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get all references targeting a given definition.
    pub fn by_target(&self, def_id: &DefId) -> &[Reference] {
        self.by_target_def.get(def_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get all references of a specific kind.
    pub fn by_kind(&self, kind: ReferenceKind) -> Vec<&Reference> {
        self.by_source_scope
            .values()
            .flat_map(|refs| refs.iter().filter(|r| r.kind == kind))
            .collect()
    }

    /// Get all call references.
    pub fn calls(&self) -> Vec<&Reference> {
        self.by_kind(ReferenceKind::Call)
    }

    /// Get all type references.
    pub fn type_refs(&self) -> Vec<&Reference> {
        self.by_kind(ReferenceKind::TypeReference)
    }

    /// Get all inheritance references.
    pub fn inheritance(&self) -> Vec<&Reference> {
        self.by_kind(ReferenceKind::Inherits)
    }

    /// Total number of references.
    pub fn len(&self) -> usize {
        self.total
    }

    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Iterate all references.
    pub fn iter(&self) -> impl Iterator<Item = &Reference> {
        self.by_source_scope.values().flat_map(|v| v.iter())
    }

    /// Iterate all source scope IDs.
    pub fn source_scopes(&self) -> impl Iterator<Item = &ScopeId> {
        self.by_source_scope.keys()
    }

    /// Iterate all target def IDs.
    pub fn target_defs(&self) -> impl Iterator<Item = &DefId> {
        self.by_target_def.keys()
    }

    /// Clear all references.
    pub fn clear(&mut self) {
        self.by_source_scope.clear();
        self.by_target_def.clear();
        self.total = 0;
    }

    /// Remove all references from a specific scope.
    pub fn remove_by_source(&mut self, scope_id: &ScopeId) -> usize {
        let Some(refs) = self.by_source_scope.remove(scope_id) else { return 0; };
        let count = refs.len();
        for r in &refs {
            if let Some(target_refs) = self.by_target_def.get_mut(&r.to_def) {
                target_refs.retain(|tr| tr.from_scope != *scope_id);
                if target_refs.is_empty() {
                    self.by_target_def.remove(&r.to_def);
                }
            }
        }
        self.total -= count;
        count
    }

    /// Remove all references to a specific definition.
    pub fn remove_by_target(&mut self, def_id: &DefId) -> usize {
        let Some(refs) = self.by_target_def.remove(def_id) else { return 0; };
        let count = refs.len();
        for r in &refs {
            if let Some(source_refs) = self.by_source_scope.get_mut(&r.from_scope) {
                source_refs.retain(|sr| sr.to_def != *def_id);
                if source_refs.is_empty() {
                    self.by_source_scope.remove(&r.from_scope);
                }
            }
        }
        self.total -= count;
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ref(from: &str, to: &str, kind: ReferenceKind) -> Reference {
        Reference {
            from_scope: from.to_string(),
            to_def: to.to_string(),
            at_range: Range { start_line: 1, start_col: 0, end_line: 1, end_col: 10 },
            kind,
            confidence: 0.9,
        }
    }

    #[test]
    fn test_add_and_query() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("scope1", "def_a", ReferenceKind::Call));
        idx.add(test_ref("scope1", "def_b", ReferenceKind::Read));
        idx.add(test_ref("scope2", "def_a", ReferenceKind::TypeReference));

        assert_eq!(idx.len(), 3);
        assert_eq!(idx.by_source(&"scope1".to_string()).len(), 2);
        assert_eq!(idx.by_target(&"def_a".to_string()).len(), 2);
        assert_eq!(idx.by_target(&"def_b".to_string()).len(), 1);
    }

    #[test]
    fn test_by_kind() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("s1", "d1", ReferenceKind::Call));
        idx.add(test_ref("s2", "d2", ReferenceKind::Call));
        idx.add(test_ref("s3", "d3", ReferenceKind::Read));

        assert_eq!(idx.calls().len(), 2);
        assert_eq!(idx.by_kind(ReferenceKind::Read).len(), 1);
        assert_eq!(idx.by_kind(ReferenceKind::Write).len(), 0);
    }

    #[test]
    fn test_type_refs() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("s1", "d1", ReferenceKind::TypeReference));
        idx.add(test_ref("s2", "d2", ReferenceKind::TypeReference));
        idx.add(test_ref("s3", "d3", ReferenceKind::Call));

        assert_eq!(idx.type_refs().len(), 2);
    }

    #[test]
    fn test_inheritance() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("s1", "d1", ReferenceKind::Inherits));
        idx.add(test_ref("s2", "d2", ReferenceKind::Call));

        assert_eq!(idx.inheritance().len(), 1);
    }

    #[test]
    fn test_remove_by_source() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("s1", "d1", ReferenceKind::Call));
        idx.add(test_ref("s1", "d2", ReferenceKind::Read));
        idx.add(test_ref("s2", "d1", ReferenceKind::Call));

        let removed = idx.remove_by_source(&"s1".to_string());
        assert_eq!(removed, 2);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.by_source(&"s1".to_string()).len(), 0);
        // d1 should still have 1 ref from s2
        assert_eq!(idx.by_target(&"d1".to_string()).len(), 1);
        // d2 should be gone
        assert_eq!(idx.by_target(&"d2".to_string()).len(), 0);
    }

    #[test]
    fn test_remove_by_target() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("s1", "d1", ReferenceKind::Call));
        idx.add(test_ref("s2", "d1", ReferenceKind::Call));
        idx.add(test_ref("s1", "d2", ReferenceKind::Read));

        let removed = idx.remove_by_target(&"d1".to_string());
        assert_eq!(removed, 2);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.by_target(&"d1".to_string()).len(), 0);
        // s1 should still have 1 ref to d2
        assert_eq!(idx.by_source(&"s1".to_string()).len(), 1);
    }

    #[test]
    fn test_iter() {
        let mut idx = ReferenceIndex::new();
        idx.add(test_ref("s1", "d1", ReferenceKind::Call));
        idx.add(test_ref("s2", "d2", ReferenceKind::Read));
        idx.add(test_ref("s3", "d3", ReferenceKind::Write));

        let all: Vec<_> = idx.iter().collect();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_add_many() {
        let mut idx = ReferenceIndex::new();
        let refs = vec![
            test_ref("s1", "d1", ReferenceKind::Call),
            test_ref("s2", "d2", ReferenceKind::Read),
            test_ref("s3", "d3", ReferenceKind::Write),
        ];
        idx.add_many(refs);
        assert_eq!(idx.len(), 3);
    }

    #[test]
    fn test_empty_index() {
        let idx = ReferenceIndex::new();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
        assert_eq!(idx.by_source(&"nonexistent".to_string()).len(), 0);
        assert_eq!(idx.by_target(&"nonexistent".to_string()).len(), 0);
    }
}
