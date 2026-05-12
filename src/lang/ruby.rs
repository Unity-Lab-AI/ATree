use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct RubyProvider;

impl LanguageProvider for RubyProvider {
    fn id(&self) -> LanguageId { LanguageId::Ruby }
    fn extensions(&self) -> &'static [&'static str] { &["rb"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_ruby::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(method name: (identifier) @name) @definition.method
(class name: (constant) @name) @definition.class
(call method: (identifier) @call.name) @call
        "#
    }
}
