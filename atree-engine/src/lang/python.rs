use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct PythonProvider;

impl LanguageProvider for PythonProvider {
    fn id(&self) -> LanguageId { LanguageId::Python }
    fn extensions(&self) -> &'static [&'static str] { &["py"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_python::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_definition name: (identifier) @name) @definition.class
(function_definition name: (identifier) @name) @definition.function
(import_statement name: (dotted_name) @import.source) @import
(import_statement name: (aliased_import name: (dotted_name) @import.source)) @import
(import_from_statement module_name: (dotted_name) @import.source) @import
(import_from_statement module_name: (relative_import) @import.source) @import
(call function: (identifier) @call.name) @call
(call function: (attribute attribute: (identifier) @call.name)) @call
(expression_statement (assignment left: (identifier) @name type: (type)) @definition.property)
(expression_statement (assignment left: (identifier) @name)) @definition.variable
(class_definition name: (identifier) @heritage.class superclasses: (argument_list (identifier) @heritage.extends)) @heritage
(assignment left: (attribute object: (_) @assignment.receiver attribute: (identifier) @assignment.property) right: (_)) @assignment
(augmented_assignment left: (attribute object: (_) @assignment.receiver attribute: (identifier) @assignment.property) right: (_)) @assignment
(decorator (call function: (attribute object: (identifier) @decorator.receiver attribute: (identifier) @decorator.name) arguments: (argument_list (string (string_content) @decorator.arg)?))) @decorator
        "#
    }
}
