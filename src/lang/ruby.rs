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
(module name: (constant) @name) @definition.module
(module name: (scope_resolution) @name) @definition.module
(class name: (constant) @name) @definition.class
(class name: (scope_resolution) @name) @definition.class
(singleton_class (constant) @name) @definition.class
(singleton_class (scope_resolution) @name) @definition.class
(method name: (identifier) @name) @definition.method
(singleton_method name: (identifier) @name) @definition.method
(call method: (identifier) @call.name) @call
(body_statement (identifier) @call.name @call)
(assignment left: (constant) @name) @definition.const
(class name: (constant) @heritage.class superclass: (superclass (constant) @heritage.extends)) @heritage
(class name: (scope_resolution) @heritage.class superclass: (superclass (scope_resolution) @heritage.extends)) @heritage
(assignment left: (call receiver: (_) @assignment.receiver method: (identifier) @assignment.property) right: (_)) @assignment
(operator_assignment left: (call receiver: (_) @assignment.receiver method: (identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
