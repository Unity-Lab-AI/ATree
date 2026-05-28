//! Evidence Extraction — AST → EvidenceCandidate pipeline.
//!
//! Stage 0 of the evidence lifecycle: parse via tree-sitter, emit candidate
//! evidence units per rule set, assign provisional IDs.
//!
//! No deduping yet. No calibration. Pure extraction.

use crate::evidence::*;
use crate::syntax::RawCapture;

/// Extract evidence candidates from a parsed file's AST captures.
///
/// Maps `RawCapture` tags to `EvidenceKind` and produces `EvidenceCandidate`s.
///
/// - `captures`: raw captures from `SyntaxEngine::extract_captures`
/// - `file_path`: absolute or repo-relative path
/// - `language`: language ID string (e.g., "typescript", "python")
/// - `enclosing_scopes`: pre-resolved scope chain (e.g., ["module", "class", "method"])
/// - `file_imports`: resolved import paths for this file
/// - `enclosing_symbol`: the innermost symbol name containing these captures
pub fn extract_from_captures(
    captures: &[RawCapture],
    file_path: &str,
    language: &str,
    enclosing_scopes: &[String],
    file_imports: &[String],
    enclosing_symbol: Option<&str>,
) -> Vec<EvidenceCandidate> {
    let mut candidates = Vec::with_capacity(captures.len());

    for capture in captures {
        let Some(kind) = capture_tag_to_kind(&capture.tag) else {
            continue;
        };

        let content = EvidenceContent {
            raw: capture.name.clone(),
            normalized: normalize_content(&capture.name, kind),
        };

        let source = EvidenceSource {
            file: file_path.to_string(),
            span: SourceSpan {
                start_line: capture.range.start_point.row,
                start_col: capture.range.start_point.column,
                end_line: capture.range.end_point.row,
                end_col: capture.range.end_point.column,
            },
            language: language.to_string(),
        };

        let target = EvidenceTarget {
            target_type: TargetType::Symbol,
            ref_id: build_target_ref(&capture.name, kind),
        };

        let context = EvidenceContext {
            enclosing_symbol: enclosing_symbol.map(String::from),
            imports: file_imports.to_vec(),
            scope_chain: enclosing_scopes.to_vec(),
        };

        let tags = build_tags(kind, &capture.call_form, &capture.receiver);

        candidates.push(EvidenceCandidate {
            kind,
            source,
            target,
            content,
            context,
            tags,
        });
    }

    candidates
}

/// Map a tree-sitter `CaptureTag` to an `EvidenceKind`.
fn capture_tag_to_kind(tag: &crate::lang::CaptureTag) -> Option<EvidenceKind> {
    use crate::lang::CaptureTag::*;
    match tag {
        // Symbol declarations
        DefinitionClass | DefinitionFunction | DefinitionMethod | DefinitionInterface
        | DefinitionEnum | DefinitionStruct | DefinitionTrait | DefinitionProperty
        | DefinitionVariable | DefinitionConst | DefinitionModule | DefinitionMacro
        | DefinitionNamespace | DefinitionConstructor | DefinitionType | DefinitionTypedef
        | DefinitionUnion | DefinitionTemplate | DefinitionAnnotation | DefinitionStatic
        | DefinitionImpl | DefinitionRecord | DefinitionDelegate => {
            Some(EvidenceKind::SymbolDeclaration)
        }

        // Calls
        CallName => Some(EvidenceKind::FunctionCall),

        // Imports
        ImportSource => Some(EvidenceKind::ImportEdge),

        // Type relations
        TypeAnnotation | HeritageExtends | HeritageImplements | HeritageTrait => {
            Some(EvidenceKind::TypeRelation)
        }

        // Data flow
        Assignment => Some(EvidenceKind::DataFlow),

        // Side effects / decorators
        Decorator | HttpClient => Some(EvidenceKind::SideEffect),

        // Heuristic inferences (unresolved / non-AST context)
        Unknown => Some(EvidenceKind::HeuristicInference),

        _ => None,
    }
}

/// Build a target reference ID from capture name and kind.
fn build_target_ref(name: &str, kind: EvidenceKind) -> String {
    match kind {
        EvidenceKind::SymbolDeclaration => format!("decl:{}", name),
        EvidenceKind::FunctionCall => format!("call:{}", name),
        EvidenceKind::ImportEdge => format!("import:{}", name),
        EvidenceKind::SymbolReference => format!("ref:{}", name),
        _ => format!("{:?}:{}", kind, name).to_lowercase(),
    }
}

/// Normalize content text based on evidence kind.
fn normalize_content(raw: &str, _kind: EvidenceKind) -> String {
    raw.trim().to_string()
}

/// Build tags from call form and receiver information.
fn build_tags(
    kind: EvidenceKind,
    call_form: &crate::syntax::CallForm,
    receiver: &Option<String>,
) -> Vec<String> {
    let mut tags = Vec::new();
    if kind == EvidenceKind::FunctionCall {
        let form_tag = match call_form {
            crate::syntax::CallForm::Free => "free_call",
            crate::syntax::CallForm::Member => "member_call",
            crate::syntax::CallForm::Constructor => "constructor",
            crate::syntax::CallForm::Scoped => "scoped_call",
            crate::syntax::CallForm::Unknown => "unclassified",
        };
        tags.push(form_tag.to_string());
        if let Some(ref recv) = receiver {
            tags.push(format!("receiver:{}", recv));
        }
    }
    if kind.is_ast_derived() {
        tags.push("ast_derived".to_string());
    } else {
        tags.push("heuristic".to_string());
    }
    tags
}

/// Check if a tree-sitter node kind represents a scope boundary.
pub fn is_scope_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "function_definition"
            | "method_definition"
            | "method_declaration"
            | "class_declaration"
            | "class_definition"
            | "interface_declaration"
            | "struct_item"
            | "impl_item"
            | "trait_item"
            | "module"
            | "namespace"
            | "class_specifier"
            | "enum_specifier"
    )
}

/// Extract a scope chain from pre-resolved scope IDs.
pub fn scope_chain_from_ids(scope_ids: &[String]) -> Vec<String> {
    scope_ids.to_vec()
}
