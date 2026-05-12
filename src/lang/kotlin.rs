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
(function_declaration (simple_identifier) @name) @definition.function
(class_declaration (type_identifier) @name) @definition.class
(call_expression (simple_identifier) @call.name) @call
(import_header (identifier) @import.source) @import
        "#
    }
}
