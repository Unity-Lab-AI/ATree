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
(call function: (identifier) @call.name) @call
(call function: (attribute attribute: (identifier) @call.name)) @call
(import_from_statement module: (dotted_name) @import.source) @import
(import_statement name: (dotted_name) @import.source) @import
        "#
    }
}
