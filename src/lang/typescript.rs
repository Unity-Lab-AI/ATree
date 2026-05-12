use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct TypeScriptProvider;

impl LanguageProvider for TypeScriptProvider {
    fn id(&self) -> LanguageId { LanguageId::TypeScript }
    fn extensions(&self) -> &'static [&'static str] { &["ts", "tsx"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration name: (type_identifier) @name) @definition.interface
(function_declaration name: (identifier) @name) @definition.function
(method_definition name: (property_identifier) @name) @definition.method
(import_statement source: (string) @import.source) @import
(call_expression function: (identifier) @call.name) @call
(call_expression function: (member_expression property: (property_identifier) @call.name)) @call
(class_declaration name: (type_identifier) @heritage.class (class_heritage (extends_clause value: (identifier) @heritage.extends))) @heritage
        "#
    }
}
