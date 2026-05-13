use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct JavaProvider;

impl LanguageProvider for JavaProvider {
    fn id(&self) -> LanguageId { LanguageId::Java }
    fn extensions(&self) -> &'static [&'static str] { &["java"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_java::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(annotation_type_declaration name: (identifier) @name) @definition.annotation
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.constructor
(field_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.property
(import_declaration (_) @import.source) @import
(method_invocation name: (identifier) @call.name) @call
(method_invocation object: (_) name: (identifier) @call.name) @call
(object_creation_expression type: (type_identifier) @call.name) @call
(local_variable_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.variable
(class_declaration name: (identifier) @heritage.class (superclass (type_identifier) @heritage.extends)) @heritage
(class_declaration name: (identifier) @heritage.class (super_interfaces (type_list (type_identifier) @heritage.implements))) @heritage.impl
(assignment_expression left: (field_access object: (_) @assignment.receiver field: (identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
