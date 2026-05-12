use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct GoProvider;

impl LanguageProvider for GoProvider {
    fn id(&self) -> LanguageId { LanguageId::Go }
    fn extensions(&self) -> &'static [&'static str] { &["go"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_go::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method
(type_spec name: (type_identifier) @name) @definition.class
(call_expression function: (identifier) @call.name) @call
(call_expression function: (selector_expression field: (field_identifier) @call.name)) @call
(import_spec path: (string_literal) @import.source) @import
        "#
    }
}
