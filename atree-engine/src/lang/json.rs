use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct JSONProvider;

impl LanguageProvider for JSONProvider {
    fn id(&self) -> LanguageId { LanguageId::JSON }
    fn extensions(&self) -> &'static [&'static str] { &["json"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_json::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(pair key: (string (string_content) @name)) @definition.property
(array (pair key: (string (string_content) @name))) @definition.property
        "#
    }
}
