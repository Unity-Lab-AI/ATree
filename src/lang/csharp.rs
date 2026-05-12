use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct CSharpProvider;

impl LanguageProvider for CSharpProvider {
    fn id(&self) -> LanguageId { LanguageId::CSharp }
    fn extensions(&self) -> &'static [&'static str] { &["cs"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_c_sharp::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(method_declaration name: (identifier) @name) @definition.method
(invocation_expression function: (identifier) @call.name) @call
(using_directive (qualified_name) @import.source) @import
        "#
    }
}
