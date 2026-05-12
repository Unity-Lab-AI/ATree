use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct CppProvider;

impl LanguageProvider for CppProvider {
    fn id(&self) -> LanguageId { LanguageId::Cpp }
    fn extensions(&self) -> &'static [&'static str] { &["cpp", "hpp", "cc", "hh"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_cpp::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(class_specifier name: (type_identifier) @name) @definition.class
(call_expression function: (identifier) @call.name) @call
(preproc_include path: (_) @import.source) @import
        "#
    }
}
