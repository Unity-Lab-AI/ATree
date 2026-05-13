use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct SwiftProvider;

impl LanguageProvider for SwiftProvider {
    fn id(&self) -> LanguageId { LanguageId::Swift }
    fn extensions(&self) -> &'static [&'static str] { &["swift"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_swift::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration "class" name: (type_identifier) @name) @definition.class
(class_declaration "struct" name: (type_identifier) @name) @definition.struct
(class_declaration "enum" name: (type_identifier) @name) @definition.enum
(class_declaration "extension" name: (user_type (type_identifier) @name)) @definition.class
(class_declaration "actor" name: (type_identifier) @name) @definition.class
(protocol_declaration name: (type_identifier) @name) @definition.interface
(typealias_declaration name: (type_identifier) @name) @definition.type
(function_declaration name: (simple_identifier) @name) @definition.function
(protocol_function_declaration name: (simple_identifier) @name) @definition.method
(init_declaration) @definition.constructor
(property_declaration (pattern (simple_identifier) @name)) @definition.property
(enum_entry (simple_identifier) @name) @definition.property
(import_declaration (identifier (simple_identifier) @import.source)) @import
(call_expression (simple_identifier) @call.name) @call
(call_expression (navigation_expression (navigation_suffix (simple_identifier) @call.name))) @call
(class_declaration name: (type_identifier) @heritage.class (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage.extends))) @heritage
(protocol_declaration name: (type_identifier) @heritage.class (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage.extends))) @heritage
(class_declaration "extension" name: (user_type (type_identifier) @heritage.class) (inheritance_specifier inherits_from: (user_type (type_identifier) @heritage.extends))) @heritage
(assignment target: (directly_assignable_expression (navigation_expression target: (_) @assignment.receiver suffix: (navigation_suffix suffix: (simple_identifier) @assignment.property))) result: (_)) @assignment
        "#
    }
}
