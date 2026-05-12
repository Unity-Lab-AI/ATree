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
(function_definition name: (name) @name) @definition.function
(class_declaration name: (name) @name) @definition.class
(method_declaration name: (name) @name) @definition.method
(function_call_expression function: (name) @call.name) @call
(namespace_use_declaration (namespace_use_clause (qualified_name) @import.source)) @import
        "#
    }
}
