use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct YAMLProvider;

impl LanguageProvider for YAMLProvider {
    fn id(&self) -> LanguageId { LanguageId::YAML }
    fn extensions(&self) -> &'static [&'static str] { &["yaml", "yml"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_yaml::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(block_mapping_pair key: (flow_node (plain_scalar (string_scalar) @name))) @definition.property
(block_mapping_pair key: (flow_node (plain_scalar (integer_scalar) @name))) @definition.property
"#
    }
}
