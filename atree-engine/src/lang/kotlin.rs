use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct KotlinProvider;

impl LanguageProvider for KotlinProvider {
    fn id(&self) -> LanguageId { LanguageId::Kotlin }
    fn extensions(&self) -> &'static [&'static str] { &["kt", "kts"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_kotlin::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration "interface" (type_identifier) @name) @definition.interface
(class_declaration "class" (type_identifier) @name) @definition.class
(object_declaration (type_identifier) @name) @definition.class
(companion_object (type_identifier) @name) @definition.class
(function_declaration (simple_identifier) @name) @definition.function
(property_declaration (variable_declaration (simple_identifier) @name)) @definition.property
(class_parameter (binding_pattern_kind) (simple_identifier) @name) @definition.property
(enum_entry (simple_identifier) @name) @definition.enum
(type_alias (type_identifier) @name) @definition.type
(import_header (identifier) @import.source) @import
(call_expression (simple_identifier) @call.name) @call
(call_expression (navigation_expression (navigation_suffix (simple_identifier) @call.name))) @call
(constructor_invocation (user_type (type_identifier) @call.name)) @call
(infix_expression (simple_identifier) @call.name) @call
(class_declaration (type_identifier) @heritage.class (delegation_specifier (user_type (type_identifier) @heritage.extends))) @heritage
(class_declaration (type_identifier) @heritage.class (delegation_specifier (constructor_invocation (user_type (type_identifier) @heritage.extends)))) @heritage
(assignment (directly_assignable_expression (_) @assignment.receiver (navigation_suffix (simple_identifier) @assignment.property)) (_)) @assignment
        "#
    }
}
