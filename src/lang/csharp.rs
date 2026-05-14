use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct CSharpProvider;

impl LanguageProvider for CSharpProvider {
    fn id(&self) -> LanguageId { LanguageId::CSharp }
    fn extensions(&self) -> &'static [&'static str] { &["cs"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_c_sharp::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(interface_declaration name: (identifier) @name) @definition.interface
(struct_declaration name: (identifier) @name) @definition.struct
(enum_declaration name: (identifier) @name) @definition.enum
(record_declaration name: (identifier) @name) @definition.record
(delegate_declaration name: (identifier) @name) @definition.delegate
(namespace_declaration name: (identifier) @name) @definition.namespace
(namespace_declaration name: (qualified_name) @name) @definition.namespace
(file_scoped_namespace_declaration name: (identifier) @name) @definition.namespace
(file_scoped_namespace_declaration name: (qualified_name) @name) @definition.namespace
(method_declaration name: (identifier) @name) @definition.method
(local_function_statement name: (identifier) @name) @definition.function
(constructor_declaration name: (identifier) @name) @definition.constructor
(property_declaration name: (identifier) @name) @definition.property
(class_declaration name: (identifier) @name (parameter_list) @definition.constructor)
(record_declaration name: (identifier) @name (parameter_list) @definition.constructor)
(using_directive (qualified_name) @import.source) @import
(using_directive (identifier) @import.source) @import
(invocation_expression function: (identifier) @call.name) @call
(invocation_expression function: (member_access_expression name: (identifier) @call.name)) @call
(invocation_expression function: (conditional_access_expression (member_binding_expression (identifier) @call.name))) @call
(object_creation_expression type: (identifier) @call.name) @call
; Note: implicit_object_creation_expression (var x = new()) has no type name — skip it
(local_declaration_statement (variable_declaration (variable_declarator (identifier) @name))) @definition.variable
(class_declaration name: (identifier) @heritage.class (base_list (identifier) @heritage.extends)) @heritage
(class_declaration name: (identifier) @heritage.class (base_list (generic_name (identifier) @heritage.extends))) @heritage
(interface_declaration name: (identifier) @heritage.class (base_list (identifier) @heritage.extends)) @heritage
(interface_declaration name: (identifier) @heritage.class (base_list (generic_name (identifier) @heritage.extends))) @heritage
(assignment_expression left: (member_access_expression expression: (_) @assignment.receiver name: (identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
