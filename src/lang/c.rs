use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct CProvider;

impl LanguageProvider for CProvider {
    fn id(&self) -> LanguageId { LanguageId::C }
    fn extensions(&self) -> &'static [&'static str] { &["c", "h"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_c::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(call_expression function: (identifier) @call.name) @call
(preproc_include path: (_) @import.source) @import
        "#
    }
}
