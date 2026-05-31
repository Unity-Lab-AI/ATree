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
;; ── Class-level declarations ──────────────────────────────────────────────
(class_declaration name: (identifier) @name) @definition.class
(enum_declaration name: (identifier) @name) @definition.enum
(mixin_declaration name: (identifier) @name) @definition.class

;; ── Functions (top-level) ────────────────────────────────────────────────
(function_signature name: (identifier) @name) @definition.function

;; ── Methods (inside class bodies) ────────────────────────────────────────
;;   method_declaration → method_signature → function_signature → name
(method_declaration (method_signature (function_signature name: (identifier) @name))) @definition.method

;; ── Constructors ─────────────────────────────────────────────────────────
(constructor_signature name: (identifier) @name) @definition.constructor
(getter_signature name: (identifier) @name) @definition.function
(setter_signature name: (identifier) @name) @definition.function

;; ── Class fields ─────────────────────────────────────────────────────────
(initialized_identifier name: (identifier) @name) @definition.property

;; ── Calls ────────────────────────────────────────────────────────────────
;;   free calls:  print("hi"), foo()
(call_expression function: (identifier) @call.name) @call
;;   method calls:  obj.method()  →  member_expression has (identifier) @ . @ (identifier)
(call_expression function: (member_expression (identifier) @call.name)) @call
;;   constructors:  new Foo()  →  new_expression has unnamed type_identifier child
(new_expression (_) @call.name) @call



;; ── Imports ──────────────────────────────────────────────────────────────
(import_specification uri: (configurable_uri (uri (string_literal) @import.source))) @import
(library_export uri: (configurable_uri (uri (string_literal) @import.source))) @import

;; ── Heritage ─────────────────────────────────────────────────────────────
(class_declaration name: (identifier) @heritage.class (superclass (type (type_identifier) @heritage.extends))) @heritage
(class_declaration name: (identifier) @heritage.class (interfaces (type (type_identifier) @heritage.implements))) @heritage.impl
(mixin_declaration name: (identifier) @heritage.class (mixins (type (type_identifier) @heritage.extends))) @heritage
        "#
    }
}
