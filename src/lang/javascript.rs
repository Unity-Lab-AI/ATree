use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct JavaScriptProvider;

impl LanguageProvider for JavaScriptProvider {
    fn id(&self) -> LanguageId { LanguageId::JavaScript }
    fn extensions(&self) -> &'static [&'static str] { &["js", "jsx", "mjs", "cjs"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_javascript::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(function_declaration name: (identifier) @name) @definition.function
(method_definition name: (property_identifier) @name) @definition.method
(import_statement source: (string) @import.source) @import
(call_expression function: (identifier) @call.name) @call
(call_expression function: (member_expression property: (property_identifier) @call.name)) @call
        "#
    }
}
