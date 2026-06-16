use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct PHPProvider;

impl LanguageProvider for PHPProvider {
    fn id(&self) -> LanguageId { LanguageId::PHP }
    fn extensions(&self) -> &'static [&'static str] { &["php"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_php::LANGUAGE_PHP.into() }
    fn query(&self) -> &'static str {
        r#"
(namespace_definition name: (namespace_name) @name) @definition.namespace
(class_declaration name: (name) @name) @definition.class
(interface_declaration name: (name) @name) @definition.interface
(trait_declaration name: (name) @name) @definition.trait
(enum_declaration name: (name) @name) @definition.enum
(function_definition name: (name) @name) @definition.function
(method_declaration name: (name) @name) @definition.method
(property_declaration (property_element (variable_name (name) @name))) @definition.property
(method_declaration parameters: (formal_parameters (property_promotion_parameter name: (variable_name (name) @name)))) @definition.property
(namespace_use_declaration (namespace_use_clause (qualified_name) @import.source)) @import
(function_call_expression function: (name) @call.name) @call
(member_call_expression name: (name) @call.name) @call
(nullsafe_member_call_expression name: (name) @call.name) @call
(scoped_call_expression name: (name) @call.name) @call
(object_creation_expression (name) @call.name) @call
(const_declaration (const_element (name) @name)) @definition.const
(class_declaration name: (name) @heritage.class (base_clause [(name) (qualified_name)] @heritage.extends)) @heritage
(class_declaration name: (name) @heritage.class (class_interface_clause [(name) (qualified_name)] @heritage.implements)) @heritage.impl
(class_declaration name: (name) @heritage.class body: (declaration_list (use_declaration [(name) (qualified_name)] @heritage.trait))) @heritage
(assignment_expression left: (member_access_expression object: (_) @assignment.receiver name: (name) @assignment.property) right: (_)) @assignment
(assignment_expression left: (scoped_property_access_expression scope: (_) @assignment.receiver name: (variable_name (name) @assignment.property)) right: (_)) @assignment
(function_call_expression function: (name) @_php_http (#match? @_php_http "^(file_get_contents|curl_init)$") arguments: (arguments (argument (string (string_content) @http_client.url)))) @http_client
        "#
    }
}
