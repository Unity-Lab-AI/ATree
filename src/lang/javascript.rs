use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct JavaScriptProvider;

impl LanguageProvider for JavaScriptProvider {
    fn id(&self) -> LanguageId { LanguageId::JavaScript }
    fn extensions(&self) -> &'static [&'static str] { &["js", "jsx", "mjs", "cjs"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_javascript::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(function_declaration name: (identifier) @name) @definition.function
(method_definition name: (property_identifier) @name) @definition.method
(method_definition name: (private_property_identifier) @name) @definition.method
(lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function))) @definition.function
(lexical_declaration (variable_declarator name: (identifier) @name value: (function_expression))) @definition.function
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function)))) @definition.function
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (function_expression)))) @definition.function
(lexical_declaration (variable_declarator name: (identifier) @name)) @definition.const
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name))) @definition.const
(variable_declaration (variable_declarator name: (identifier) @name)) @definition.variable
(import_statement source: (string) @import.source) @import
(export_statement source: (string) @import.source) @import
(call_expression function: (identifier) @call.name) @call
(call_expression function: (member_expression property: (property_identifier) @call.name)) @call
(new_expression constructor: (identifier) @call.name) @call
(field_definition property: (property_identifier) @name) @definition.property
(class_declaration name: (identifier) @heritage.class (class_heritage (identifier) @heritage.extends)) @heritage
(assignment_expression left: (member_expression object: (_) @assignment.receiver property: (property_identifier) @assignment.property) right: (_)) @assignment
(augmented_assignment_expression left: (member_expression object: (_) @assignment.receiver property: (property_identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
