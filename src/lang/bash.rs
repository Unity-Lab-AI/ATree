use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct BashProvider;

impl LanguageProvider for BashProvider {
    fn id(&self) -> LanguageId { LanguageId::Bash }
    fn extensions(&self) -> &'static [&'static str] { &["sh", "bash"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_bash::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(function_definition name: (word) @name) @definition.function
(command name: (command_name (word) @call.name)) @call
(variable_assignment name: (variable_name) @name) @definition.variable
(for_statement (word) @name) @definition.variable
        "#
    }
}
