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
(function_declaration name: (simple_identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(call_expression (simple_identifier) @call.name) @call
(import_declaration (identifier (simple_identifier) @import.source)) @import
        "#
    }
}
