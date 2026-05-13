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
(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method
(type_declaration (type_spec name: (type_identifier) @name type: (struct_type))) @definition.struct
(type_declaration (type_spec name: (type_identifier) @name type: (interface_type))) @definition.interface
(import_declaration (import_spec path: (interpreted_string_literal) @import.source)) @import
(import_declaration (import_spec_list (import_spec path: (interpreted_string_literal) @import.source))) @import
(field_declaration_list (field_declaration name: (field_identifier) @name) @definition.property)
(type_declaration (type_spec name: (type_identifier) @heritage.class type: (struct_type (field_declaration_list (field_declaration type: (type_identifier) @heritage.extends))))) @definition.struct
(call_expression function: (identifier) @call.name) @call
(call_expression function: (selector_expression field: (field_identifier) @call.name)) @call
(const_declaration (const_spec name: (identifier) @name)) @definition.const
(var_declaration (var_spec name: (identifier) @name)) @definition.variable
(short_var_declaration left: (expression_list (identifier) @name)) @definition.variable
(composite_literal type: (type_identifier) @call.name) @call
(assignment_statement left: (expression_list (selector_expression operand: (_) @assignment.receiver field: (field_identifier) @assignment.property)) right: (_)) @assignment
(inc_statement (selector_expression operand: (_) @assignment.receiver field: (field_identifier) @assignment.property)) @assignment
(dec_statement (selector_expression operand: (_) @assignment.receiver field: (field_identifier) @assignment.property)) @assignment
        "#
    }
}
