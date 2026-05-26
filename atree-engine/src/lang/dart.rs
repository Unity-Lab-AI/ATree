use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct DartProvider;

impl LanguageProvider for DartProvider {
    fn id(&self) -> LanguageId { LanguageId::Dart }
    fn extensions(&self) -> &'static [&'static str] { &["dart"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_dart::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_declaration name: (identifier) @name) @definition.class
(enum_declaration name: (identifier) @name) @definition.enum
(mixin_declaration name: (identifier) @name) @definition.class
(function_signature name: (identifier) @name) @definition.function
(constructor_signature name: (identifier) @name) @definition.constructor
(getter_signature name: (identifier) @name) @definition.function
(setter_signature name: (identifier) @name) @definition.function
(import_specification uri: (configurable_uri (uri (string_literal) @import.source))) @import
(library_export uri: (configurable_uri (uri (string_literal) @import.source))) @import
(class_declaration name: (identifier) @heritage.class (superclass (type (type_identifier) @heritage.extends))) @heritage
(class_declaration name: (identifier) @heritage.class (interfaces (type (type_identifier) @heritage.implements))) @heritage.impl
(mixin_declaration name: (identifier) @heritage.class (mixins (type (type_identifier) @heritage.extends))) @heritage
        "#
    }
}
