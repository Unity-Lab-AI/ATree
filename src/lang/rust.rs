use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct RustProvider;

impl LanguageProvider for RustProvider {
    fn id(&self) -> LanguageId { LanguageId::Rust }
    fn extensions(&self) -> &'static [&'static str] { &["rs"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_rust::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(function_item name: (identifier) @name) @definition.function
(struct_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(trait_item name: (type_identifier) @name) @definition.trait
(call_expression function: (identifier) @call.name) @call
(call_expression function: (field_expression field: (field_identifier) @call.name)) @call
(use_declaration argument: (_) @import.source) @import
        "#
    }
}
