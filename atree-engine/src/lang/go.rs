use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct GoProvider;

impl LanguageProvider for GoProvider {
    fn id(&self) -> LanguageId { LanguageId::Go }
    fn extensions(&self) -> &'static [&'static str] { &["go"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_go::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
;; ── Functions & Methods ──────────────────────────────────────────────
(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method

;; ── Types (struct/interface) — capture on type_spec to avoid duplicate ──
(type_spec name: (type_identifier) @name type: (struct_type)) @definition.struct
(type_spec name: (type_identifier) @name type: (interface_type)) @definition.interface

;; ── Imports — handle both single and grouped import declarations ─────
(import_spec path: (interpreted_string_literal) @import.source) @import

;; ── Struct fields ────────────────────────────────────────────────────
(field_declaration name: (field_identifier) @name) @definition.property

;; ── Calls ────────────────────────────────────────────────────────────
(call_expression function: (identifier) @call.name) @call
(call_expression function: (selector_expression field: (field_identifier) @call.name)) @call
(composite_literal type: (type_identifier) @call.name) @call

;; ── Constants & Variables ────────────────────────────────────────────
(const_declaration (const_spec name: (identifier) @name)) @definition.const
(var_declaration (var_spec name: (identifier) @name)) @definition.variable
(short_var_declaration left: (expression_list (identifier) @name)) @definition.variable

;; ── Assignments (selector expressions: obj.field = value) ─────────────
(assignment_statement left: (expression_list (selector_expression operand: (_) @assignment.receiver field: (field_identifier) @assignment.property)) right: (_)) @assignment
(inc_statement (selector_expression operand: (_) @assignment.receiver field: (field_identifier) @assignment.property)) @assignment
(dec_statement (selector_expression operand: (_) @assignment.receiver field: (field_identifier) @assignment.property)) @assignment
        "#
    }
}
