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
(function_signature_item name: (identifier) @name) @definition.function
(struct_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(trait_item name: (type_identifier) @name) @definition.trait
(impl_item type: (type_identifier) @name !trait) @definition.impl
(impl_item type: (generic_type type: (type_identifier) @name) !trait) @definition.impl
(mod_item name: (identifier) @name) @definition.module
(type_item name: (type_identifier) @name) @definition.type
(const_item name: (identifier) @name) @definition.const
(static_item name: (identifier) @name) @definition.static
(macro_definition name: (identifier) @name) @definition.macro
(use_declaration argument: (_) @import.source) @import
(call_expression function: (identifier) @call.name) @call
(call_expression function: (field_expression field: (field_identifier) @call.name)) @call
(call_expression function: (scoped_identifier name: (identifier) @call.name)) @call
(call_expression function: (generic_function function: (identifier) @call.name)) @call
(struct_expression name: (type_identifier) @call.name) @call
(field_declaration_list (field_declaration name: (field_identifier) @name) @definition.property)
(impl_item trait: (type_identifier) @heritage.trait type: (type_identifier) @heritage.class) @heritage
(impl_item trait: (generic_type type: (type_identifier) @heritage.trait) type: (type_identifier) @heritage.class) @heritage
(impl_item trait: (type_identifier) @heritage.trait type: (generic_type type: (type_identifier) @heritage.class)) @heritage
(impl_item trait: (generic_type type: (type_identifier) @heritage.trait) type: (generic_type type: (type_identifier) @heritage.class)) @heritage
(assignment_expression left: (field_expression value: (_) @assignment.receiver field: (field_identifier) @assignment.property) right: (_)) @assignment
(compound_assignment_expr left: (field_expression value: (_) @assignment.receiver field: (field_identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
