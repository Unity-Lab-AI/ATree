use super::{LanguageId, LanguageProvider};
use tree_sitter::Language;

#[derive(Debug)]
pub struct CppProvider;

impl LanguageProvider for CppProvider {
    fn id(&self) -> LanguageId { LanguageId::Cpp }
    fn extensions(&self) -> &'static [&'static str] { &["cpp", "hpp", "cc", "hh"] }
    fn tree_sitter_language(&self) -> Language { tree_sitter_cpp::LANGUAGE.into() }
    fn query(&self) -> &'static str {
        r#"
(class_specifier name: (type_identifier) @name) @definition.class
(struct_specifier name: (type_identifier) @name) @definition.struct
(namespace_definition name: (namespace_identifier) @name) @definition.namespace
(enum_specifier name: (type_identifier) @name) @definition.enum
(type_definition declarator: (type_identifier) @name) @definition.typedef
(union_specifier name: (type_identifier) @name) @definition.union
(preproc_function_def name: (identifier) @name) @definition.macro
(preproc_def name: (identifier) @name) @definition.macro
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @definition.method
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name)))) @definition.method
(function_definition declarator: (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))) @definition.function
(function_definition declarator: (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))))) @definition.method
(function_definition declarator: (reference_declarator (function_declarator declarator: (identifier) @name))) @definition.function
(function_definition declarator: (reference_declarator (function_declarator declarator: (qualified_identifier name: (identifier) @name)))) @definition.method
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (destructor_name) @name))) @definition.method
(declaration declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(declaration declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(field_declaration declarator: (field_identifier) @name) @definition.property
(field_declaration declarator: (pointer_declarator declarator: (field_identifier) @name)) @definition.property
(field_declaration declarator: (reference_declarator (field_identifier) @name)) @definition.property
(field_declaration declarator: (function_declarator declarator: [(field_identifier) (identifier)] @name)) @definition.method
(field_declaration declarator: (pointer_declarator declarator: (function_declarator declarator: [(field_identifier) (identifier)] @name))) @definition.method
(field_declaration declarator: (reference_declarator (function_declarator declarator: [(field_identifier) (identifier)] @name))) @definition.method
(field_declaration_list (function_definition declarator: (function_declarator declarator: [(field_identifier) (identifier) (operator_name) (destructor_name)] @name)) @definition.method)
(field_declaration_list (function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: [(field_identifier) (identifier) (operator_name)] @name))) @definition.method)
(field_declaration_list (function_definition declarator: (reference_declarator (function_declarator declarator: [(field_identifier) (identifier) (operator_name)] @name))) @definition.method)
(template_declaration (class_specifier name: (type_identifier) @name)) @definition.template
(template_declaration (function_definition declarator: (function_declarator declarator: (identifier) @name))) @definition.template
(preproc_include path: (_) @import.source) @import
(call_expression function: (identifier) @call.name) @call
(call_expression function: (field_expression field: (field_identifier) @call.name)) @call
(call_expression function: (qualified_identifier name: (identifier) @call.name)) @call
(call_expression function: (template_function name: (identifier) @call.name)) @call
(new_expression type: (type_identifier) @call.name) @call
(declaration declarator: (init_declarator declarator: (identifier) @name)) @definition.variable
(class_specifier name: (type_identifier) @heritage.class (base_class_clause (type_identifier) @heritage.extends)) @heritage
(class_specifier name: (type_identifier) @heritage.class (base_class_clause (access_specifier) (type_identifier) @heritage.extends)) @heritage
(assignment_expression left: (field_expression argument: (_) @assignment.receiver field: (field_identifier) @assignment.property) right: (_)) @assignment
        "#
    }
}
