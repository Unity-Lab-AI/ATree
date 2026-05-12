use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct JavaProvider;

impl LanguageProvider for JavaProvider {
    fn id(&self) -> LanguageId { LanguageId::Java }
    fn extensions(&self) -> &'static [&'static str] { &["java"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_java::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.function
(import_declaration (scoped_identifier) @import.source) @import
(method_invocation name: (identifier) @call.name) @call
        "#
    }
}
