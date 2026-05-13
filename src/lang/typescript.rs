use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct TypeScriptProvider;

impl LanguageProvider for TypeScriptProvider {
    fn id(&self) -> LanguageId { LanguageId::TypeScript }
    fn extensions(&self) -> &'static [&'static str] { &["ts", "tsx"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration name: (type_identifier) @name) @definition.interface
(function_declaration name: (identifier) @name) @definition.function
(function_signature name: (identifier) @name) @definition.function
(method_definition name: (property_identifier) @name) @definition.method
(method_definition name: (private_property_identifier) @name) @definition.method
(abstract_method_signature name: (property_identifier) @name) @definition.method
(method_signature name: (property_identifier) @name) @definition.method
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
(call_expression function: (await_expression (identifier) @call.name) (type_arguments)) @call
(call_expression function: (await_expression (member_expression property: (property_identifier) @call.name)) (type_arguments)) @call
(new_expression constructor: (identifier) @call.name) @call
(public_field_definition name: (property_identifier) @name) @definition.property
(public_field_definition name: (private_property_identifier) @name) @definition.property
(required_parameter (accessibility_modifier) pattern: (identifier) @name) @definition.property
(class_declaration name: (type_identifier) @heritage.class (class_heritage (extends_clause value: (identifier) @heritage.extends))) @heritage
(class_declaration name: (type_identifier) @heritage.class (class_heritage (implements_clause (type_identifier) @heritage.implements))) @heritage.impl
(assignment_expression left: (member_expression object: (_) @assignment.receiver property: (property_identifier) @assignment.property) right: (_)) @assignment
(augmented_assignment_expression left: (member_expression object: (_) @assignment.receiver property: (property_identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
