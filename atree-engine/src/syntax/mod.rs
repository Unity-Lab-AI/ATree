use streaming_iterator::StreamingIterator;
use serde::{Serialize, Deserialize};
use tree_sitter::{Parser, Query, QueryCursor, Node, Tree};
use crate::lang::{LanguageProvider, CaptureTag, detect_visibility};
use crate::semantic::ScopeKind;

pub struct SyntaxEngine {
    parser: Parser,
    cursor: QueryCursor,
    /// Cached compiled query for the current language. When the language changes,
    /// this is recompiled once and reused for all subsequent files in that language.
    cached_query: Option<Query>,
}

impl SyntaxEngine {
    /// Set the parser language and compile the query for this language.
    /// Call this once before parsing a batch of same-language files.
    /// The compiled query is cached and reused, avoiding per-file Query::new() overhead.
    pub fn set_language_for(&mut self, provider: &dyn LanguageProvider) {
        let _ = self.parser.set_language(&provider.tree_sitter_language());
        // Pre-compile the query once per language.
        self.cached_query = Query::new(&provider.tree_sitter_language(), provider.query())
            .map_err(|e| {
                tracing::debug!(lang = ?provider.id(), error = %e, "Query compilation failed for language");
                e
            })
            .ok();
    }

    /// Parse content using the already-configured language.
    /// Call set_language_for() first.
    pub fn parse_only(&mut self, content: &str) -> Option<Tree> {
        self.parser.parse(content, None)
    }

    /// Reset the parser for a new language and content.
    pub fn reset_for(&mut self, provider: &dyn LanguageProvider, content: &str) -> Option<Tree> {
        self.set_language_for(provider);
        self.parser.parse(content, None)
    }
}

/// Compute a fast hash of file content for change detection.
#[inline]
pub fn hash_content(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

/// Classified call form — how the call site is structured in the AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CallForm {
    /// Free/unqualified call: `foo()`
    Free,
    /// Member/instance call: `obj.method()`, `this.method()`
    Member,
    /// Constructor call: `new Foo()`, `Foo()`
    Constructor,
    /// Scoped/qualified call: `Foo::bar()`, `ns::func()`
    Scoped,
    /// Unknown/unclassified
    #[default]
    Unknown,
}


#[derive(Debug)]
pub struct RawCapture {
    pub tag: CaptureTag,
    pub name: String,
    pub range: tree_sitter::Range,
    /// For CallName captures: the classified call form.
    pub call_form: CallForm,
    /// For Member/Constructor calls: the receiver expression text (e.g., "self", "obj", "new").
    pub receiver: Option<String>,
    /// For TypeAnnotation captures: the variable/parameter name being typed.
    /// When present, `name` holds the type text and `related_name` holds the binding target.
    pub related_name: Option<String>,
    /// Visibility modifier detected from source text preceding the definition node.
    /// Populated for definition captures by scanning the line for keywords like
    /// pub, export, public, private, protected, internal, open.
    pub visibility: Option<String>,
}

/// A type binding extracted from the AST: a variable/parameter and its type annotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeBinding {
    /// The variable/parameter name (e.g., "user", "index").
    pub var_name: String,
    /// The type text (e.g., "User", "number", "string | null").
    pub type_text: String,
    /// Line number of the binding.
    pub line: usize,
    /// The AST node kind that owns this binding (e.g., "variable_declarator", "formal_parameter").
    pub owner_kind: String,
}

/// A scope node extracted from the AST during the walk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawScope {
    pub kind: ScopeKind,
    pub line_start: usize,
    pub line_end: usize,
    pub parent_idx: Option<usize>, // index into the scope stack during extraction
}

impl Default for SyntaxEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxEngine {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            cursor: QueryCursor::new(),
            cached_query: None,
        }
    }

    pub fn extract_captures(&mut self, provider: &dyn LanguageProvider, content: &str) -> Vec<RawCapture> {
        let tree = match self.reset_for(provider, content) {
            Some(t) => t,
            None => return Vec::new(),
        };
        self.extract_captures_from_tree(provider, content, &tree)
    }

    /// Extract captures from an already-parsed tree-sitter tree.
    /// Avoids re-parsing when the caller already has a Tree.
    fn extract_captures_from_tree(
        &mut self,
        _provider: &dyn LanguageProvider,
        content: &str,
        tree: &Tree,
    ) -> Vec<RawCapture> {
        let query = match self.cached_query.as_ref() {
            Some(q) => q,
            None => return Vec::new(),
        };

        let capture_names = query.capture_names();
        self.cursor.set_byte_range(0..content.len());
        let mut matches = self.cursor.matches(&query, tree.root_node(), content.as_bytes());

        let mut captures = Vec::new();
        // Includes capture index to avoid deduplicating captures at the same position
        // but with different tags (e.g., ImportSource + ImportWrapper at same byte range).
        let mut seen = std::collections::HashSet::<(String, usize, usize, usize)>::new();

        while let Some(m) = matches.next() {
            // Collect all captures in this match with their metadata.
            // Keep @name captures separate — they provide the identifier text.
            let mut name_capture_idx: Option<usize> = None;
            let mut semantic_captures: Vec<(usize, CaptureTag)> = Vec::new();

            for c in m.captures.iter() {
                let tag_name = capture_names[c.index as usize];
                let tag = CaptureTag::from(tag_name);
                if tag_name == "name" {
                    // Plain @name — just the identifier text, no semantic meaning
                    name_capture_idx = Some(c.index as usize);
                } else if tag_name.ends_with(".name") {
                    // @call.name etc. — serves as BOTH name text and semantic tag
                    name_capture_idx = Some(c.index as usize);
                    if tag != CaptureTag::Unknown {
                        semantic_captures.push((c.index as usize, tag));
                    }
                } else if tag != CaptureTag::Unknown {
                    semantic_captures.push((c.index as usize, tag));
                }
            }

            if let Some(name_idx) = name_capture_idx {
                let name_capture = m
                    .captures
                    .iter()
                    .find(|c| c.index as usize == name_idx)
                    .expect("name capture must exist in match");
                let name_text =
                    &content[name_capture.node.start_byte()..name_capture.node.end_byte()];
                let name_range = name_capture.node.range();

                // Find the @call wrapper node for call form analysis
                let call_node = m.captures.iter()
                    .find(|c| {
                        let tn = capture_names[c.index as usize];
                        tn == "call" || tn == "call_expression" || tn == "invocation_expression"
                            || tn == "method_invocation" || tn == "member_call_expression"
                            || tn == "nullsafe_member_call_expression" || tn == "scoped_call_expression"
                            || tn == "new_expression" || tn == "object_creation_expression"
                    })
                    .map(|c| c.node);

                for &(capture_idx, ref tag) in &semantic_captures {
                    match tag {
                        CaptureTag::CallWrapper
                        | CaptureTag::ImportWrapper
                        | CaptureTag::HeritageWrapper => continue,
                        _ => {}
                    }
                    let key = (
                        name_text.to_string(),
                        name_range.start_byte,
                        name_range.end_byte,
                        capture_idx,
                    );
                    if seen.insert(key) {
                        let (call_form, receiver) = if *tag == CaptureTag::CallName {
                            classify_call_form(name_capture.node, call_node, content)
                        } else {
                            (CallForm::Unknown, None)
                        };
                        let visibility = if tag.is_definition() {
                            detect_visibility(content, name_capture.node)
                        } else {
                            None
                        };
                        captures.push(RawCapture {
                            tag: *tag,
                            name: name_text.to_string(),
                            range: name_range,
                            call_form,
                            receiver,
                            related_name: None,
                            visibility,
                        });
                    }
                }
            } else {
                // No @name capture — use the capture text directly
                for &(idx, ref tag) in &semantic_captures {
                    let c = m.captures.iter().find(|c| c.index as usize == idx)
                        .expect("semantic capture must exist in match");
                    let text = &content[c.node.start_byte()..c.node.end_byte()];
                    let key = (text.to_string(), c.node.start_byte(), c.node.end_byte(), idx);
                    if seen.insert(key) {
                        let visibility = if tag.is_definition() {
                            detect_visibility(content, c.node)
                        } else {
                            None
                        };
                        captures.push(RawCapture {
                            tag: *tag,
                            name: text.to_string(),
                            range: c.node.range(),
                            call_form: CallForm::Unknown,
                            receiver: None, related_name: None, visibility,
                        });
                    }
                }
            }
        }

    /// Classify a call capture by inspecting the AST relationship between
    /// the name node and its parent call node.
    /// Ported from GitNexus call-analysis.ts inferCallForm().
    fn classify_call_form(
        name_node: Node,
        call_node: Option<Node>,
        content: &str,
    ) -> (CallForm, Option<String>) {
        let call = match call_node {
            Some(n) => n,
            None => return (CallForm::Unknown, None),
        };

        // Constructor: call node is a new_expression or object_creation_expression
        let call_kind = call.kind();
        if call_kind == "new_expression" || call_kind == "object_creation_expression"
            || call_kind == "constructor_invocation" || call_kind == "struct_expression"
            || call_kind == "composite_literal"
        {
            return (CallForm::Constructor, None);
        }

        // Check if name_node is inside a member-access wrapper
        let name_parent = match name_node.parent() {
            Some(p) => p,
            None => return (CallForm::Free, None),
        };

        let parent_kind = name_parent.kind();
        let is_member = matches!(
            parent_kind,
            "member_expression" | "attribute" | "member_access_expression"
                | "field_expression" | "selector_expression" | "navigation_suffix"
                | "member_binding_expression" | "field_access" | "scoped_identifier"
        );

        if is_member {
            // Extract the receiver (object) from the member access
            let receiver = if let Some(obj) = name_parent.child_by_field_name("object") {
                let text = &content[obj.start_byte()..obj.end_byte()];
                Some(text.to_string())
            } else if let Some(obj) = name_parent.child(0) {
                let text = &content[obj.start_byte()..obj.end_byte()];
                Some(text.to_string())
            } else {
                None
            };
            return (CallForm::Member, receiver);
        }

        // PHP member calls
        if call_kind == "member_call_expression" || call_kind == "nullsafe_member_call_expression" {
            return (CallForm::Member, None);
        }

        // Java method_invocation with object field
        if call_kind == "method_invocation" && call.child_by_field_name("object").is_some() {
            if let Some(obj) = call.child_by_field_name("object") {
                let text = &content[obj.start_byte()..obj.end_byte()];
                return (CallForm::Member, Some(text.to_string()));
            }
            return (CallForm::Member, None);
        }

        // Ruby call with receiver
        if call_kind == "call" && call.child_by_field_name("receiver").is_some() {
            if let Some(rcv) = call.child_by_field_name("receiver") {
                let text = &content[rcv.start_byte()..rcv.end_byte()];
                return (CallForm::Member, Some(text.to_string()));
            }
            return (CallForm::Member, None);
        }

        // Scoped calls (Rust Foo::new(), C++ ns::func())
        // The name node itself may be a scoped_identifier, or its parent may be one
        let name_kind = name_node.kind();
        if name_kind == "scoped_identifier" || name_kind == "qualified_identifier"
            || parent_kind == "scoped_identifier" || parent_kind == "qualified_identifier"
        {
            return (CallForm::Scoped, None);
        }

        // Default: free call
        (CallForm::Free, None)
    }

        captures
    }

    /// Extract both captures and scope tree from a file.
    /// This is the main entry point for semantic analysis.
    /// Reuses the internal parser across calls (reset per file).
    pub fn extract_captures_and_scopes(
        &mut self,
        provider: &dyn LanguageProvider,
        content: &str,
    ) -> (Vec<RawCapture>, Vec<RawScope>, Vec<TypeBinding>) {
        let tree = match self.reset_for(provider, content) {
            Some(t) => t,
            None => return (Vec::new(), Vec::new(), Vec::new()),
        };

        self.extract_from_tree(provider, content, &tree)
    }

    /// Extract captures, scopes, and type bindings from content using an
    /// already-configured language. Call set_language_for() first.
    /// This avoids the per-file set_language() overhead when processing
    /// batches of same-language files.
    pub fn extract_captures_and_scopes_preloaded(
        &mut self,
        provider: &dyn LanguageProvider,
        content: &str,
    ) -> (Vec<RawCapture>, Vec<RawScope>, Vec<TypeBinding>) {
        let tree = match self.parse_only(content) {
            Some(t) => t,
            None => return (Vec::new(), Vec::new(), Vec::new()),
        };

        self.extract_from_tree(provider, content, &tree)
    }

    /// Shared extraction logic: captures + scopes + type bindings from a parsed tree.
    fn extract_from_tree(
        &mut self,
        provider: &dyn LanguageProvider,
        content: &str,
        tree: &Tree,
    ) -> (Vec<RawCapture>, Vec<RawScope>, Vec<TypeBinding>) {

        // Extract captures using the already-parsed tree (avoids double-parse)
        let captures = self.extract_captures_from_tree(provider, content, &tree);

        // Walk the AST to extract scope tree and type bindings
        let (scopes, type_bindings) = self.extract_scopes_and_types(&tree, content, provider.id());

        (captures, scopes, type_bindings)
    }

    /// Walk the tree-sitter AST and extract scope-defining nodes + type bindings.
    /// Returns (scopes, type_bindings) where type_bindings are extracted from
    /// AST nodes that have both a name child and a type annotation child.
    fn extract_scopes_and_types(
        &self,
        tree: &Tree,
        content: &str,
        lang: crate::lang::LanguageId,
    ) -> (Vec<RawScope>, Vec<TypeBinding>) {
        let root = tree.root_node();
        let mut scopes: Vec<RawScope> = Vec::new();
        let mut type_bindings: Vec<TypeBinding> = Vec::new();
        // Stack of (node, scope_index_in_scopes_vec)
        let mut scope_stack: Vec<(Node, usize)> = Vec::new();

        // The root node is always the module scope
        let module_scope = RawScope {
            kind: ScopeKind::Module,
            line_start: root.start_position().row,
            line_end: root.end_position().row,
            parent_idx: None,
        };
        scopes.push(module_scope);
        scope_stack.push((root, 0));

        self.walk_node(root, content, lang, &mut scopes, &mut scope_stack, &mut type_bindings);

        (scopes, type_bindings)
    }

    fn walk_node<'a>(
        &self,
        node: Node<'a>,
        content: &str,
        lang: crate::lang::LanguageId,
        scopes: &mut Vec<RawScope>,
        scope_stack: &mut Vec<(Node<'a>, usize)>,
        type_bindings: &mut Vec<TypeBinding>,
    ) {
        // Check if this node creates a new scope
        if let Some(scope_kind) = self.node_scope_kind(node, lang) {
            let parent_idx = scope_stack.last().map(|(_, idx)| *idx);
            let scope_idx = scopes.len();
            scopes.push(RawScope {
                kind: scope_kind,
                line_start: node.start_position().row,
                line_end: node.end_position().row,
                parent_idx,
            });
            scope_stack.push((node, scope_idx));
        }

        // Extract type bindings from this node if it has both a name and a type annotation
        self.extract_type_binding(node, content, lang, type_bindings);

        // Recurse into children
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.walk_node(cursor.node(), content, lang, scopes, scope_stack, type_bindings);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }

        // Pop scope stack if this node created a scope
        if self.node_scope_kind(node, lang).is_some() {
            scope_stack.pop();
        }
    }

    /// Extract a type binding from a single AST node.
    /// Looks for nodes that have both a name child (the variable/parameter name)
    /// and a type annotation child (the type text).
    fn extract_type_binding(
        &self,
        node: Node,
        content: &str,
        lang: crate::lang::LanguageId,
        type_bindings: &mut Vec<TypeBinding>,
    ) {
        let node_kind = node.kind();
        let mut var_name: Option<String> = None;
        let mut type_text: Option<String> = None;

        // Language-specific extraction based on AST structure
        match lang {
            crate::lang::LanguageId::TypeScript | crate::lang::LanguageId::JavaScript => {
                self.extract_ts_js_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Python => {
                self.extract_python_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Rust => {
                self.extract_rust_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Go => {
                self.extract_go_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Java => {
                self.extract_java_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::C => {
                // C: type comes from declaration, not annotation — handled by declaration kind
            }
            crate::lang::LanguageId::Cpp => {
                self.extract_cpp_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::CSharp => {
                self.extract_csharp_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::PHP => {
                self.extract_php_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Ruby => {
                // Ruby is dynamically typed — no type annotations in standard Ruby
            }
            crate::lang::LanguageId::Kotlin => {
                self.extract_kotlin_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Swift => {
                self.extract_swift_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Dart => {
                self.extract_dart_type_binding(node, content, node_kind, &mut var_name, &mut type_text);
            }
            crate::lang::LanguageId::Bash
            | crate::lang::LanguageId::JSON
            | crate::lang::LanguageId::YAML
            | crate::lang::LanguageId::Unknown => {}
        }

        if let (Some(name), Some(ty)) = (var_name, type_text) {
            let trimmed = ty.trim();
            if !trimmed.is_empty() {
                type_bindings.push(TypeBinding {
                    var_name: name,
                    type_text: trimmed.to_string(),
                    line: node.start_position().row,
                    owner_kind: node_kind.to_string(),
                });
            }
        }
    }

    /// TypeScript/JavaScript type binding extraction.
    /// Handles: variable_declarator, formal_parameter, property_signature, public_field_definition,
    /// method_definition (return type), function_declaration (return type).
    fn extract_ts_js_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // const x: Type = ... — name is the identifier, type is type_annotation
            "variable_declarator" | "property_definition" | "property_signature"
            | "public_field_definition" | "private_field_definition" | "protected_field_definition" => {
                *var_name = node.child_by_field_name( "name")
                    .or_else(|| node.child_by_field_name( "pattern"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // function foo(x: Type): ReturnType — formal parameters have type annotations
            "formal_parameter" | "required_parameter" | "optional_parameter" => {
                *var_name = node.child_by_field_name( "pattern")
                    .or_else(|| node.child_by_field_name( "name"))
                    .or_else(|| node.child_by_field_name( "left"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // function foo(): ReturnType — return type annotation
            "function_declaration" | "function_signature" | "method_definition"
            | "method_signature" | "arrow_function" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "return_type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Python type binding extraction.
    /// Handles: typed_parameter, parameter (with type comment), assignment with type annotation.
    fn extract_python_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // def foo(x: Type) — typed_parameter
            "typed_parameter" => {
                // The name is the first identifier child
                *var_name = node.children(&mut node.walk())
                    .find(|c| c.kind() == "identifier")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // x: Type = ... — assignment with type annotation
            "assignment" => {
                *var_name = node.child_by_field_name( "left")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // def foo() -> ReturnType — return type
            "function_definition" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "return_type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Rust type binding extraction.
    /// Handles: function_item (return type), let_declaration, field_declaration,
    /// function_parameter.
    fn extract_rust_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // let x: Type = ...
            "let_declaration" => {
                *var_name = node.child_by_field_name( "pattern")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // fn foo() -> ReturnType
            "function_item" | "function_signature_item" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "return_type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // param: Type
            "function_parameter" => {
                *var_name = node.child_by_field_name( "pattern")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // struct field: name: Type
            "field_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Go type binding extraction.
    /// Handles: var_spec, const_spec, parameter_declaration, function_declaration (return type).
    fn extract_go_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // var x Type  or  x := expr (type from value)
            "var_spec" | "const_spec" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // func(param Type) — parameter_declaration
            "parameter_declaration" | "variadic_parameter_declaration" => {
                // Go params can have multiple names for one type: a, b int
                let names: Vec<String> = node.children(&mut node.walk())
                    .filter(|c| c.kind() == "identifier")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string())
                    .collect();
                if !names.is_empty() {
                    *var_name = Some(names.join(", "));
                }
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // func foo() ReturnType — result type
            "function_declaration" | "method_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "result")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Java type binding extraction.
    /// Handles: local_variable_declaration, field_declaration, formal_parameter,
    /// method_declaration (return type).
    fn extract_java_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // Type x = ...
            "local_variable_declaration" | "field_declaration" => {
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                // The declarator has the variable name
                if let Some(decl) = node.child_by_field_name( "declarator") {
                    *var_name = decl.child_by_field_name( "name")
                        .or_else(|| decl.child_by_field_name( "pattern"))
                        .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                }
            }
            // void foo(Type param) — formal_parameter
            "formal_parameter" => {
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *var_name = node.child_by_field_name( "name")
                    .or_else(|| node.child_by_field_name( "pattern"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // Type foo() — method return type
            "method_declaration" | "constructor_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// C++ type binding extraction.
    /// Handles: parameter_declaration, field_declaration, function_definition (return type).
    fn extract_cpp_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            "parameter_declaration" => {
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *var_name = node.child_by_field_name( "declarator")
                    .map(|d| content[d.start_byte()..d.end_byte()].to_string());
            }
            "field_declaration" => {
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                if let Some(decl) = node.child_by_field_name( "declarator") {
                    *var_name = Some(content[decl.start_byte()..decl.end_byte()].to_string());
                }
            }
            "function_definition" => {
                *var_name = node.child_by_field_name( "declarator")
                    .map(|d| content[d.start_byte()..d.end_byte()].to_string());
                *type_text = node.child_by_field_name( "return_type")
                    .or_else(|| node.child_by_field_name( "type"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// C# type binding extraction.
    /// Handles: variable_declaration, parameter, property_declaration,
    /// method_declaration (return type).
    fn extract_csharp_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // Type x = ...
            "variable_declaration" => {
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                if let Some(decl) = node.child_by_field_name( "declarator") {
                    *var_name = decl.child_by_field_name( "name")
                        .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                }
            }
            // Type param
            "parameter" => {
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // Type Property { get; set; }
            "property_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // Type Method() — return type
            "method_declaration" | "constructor_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// PHP type binding extraction.
    /// Handles: parameter (typed), property_element (typed), function_definition (return type).
    fn extract_php_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // function foo(Type $param)
            "parameter" => {
                *type_text = node.child_by_field_name( "type")
                    .or_else(|| node.child_by_field_name( "type_clause"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // private Type $prop;
            "property_element" => {
                // type_clause or type_list
                *type_text = node.child_by_field_name( "type")
                    .or_else(|| node.child_by_field_name( "type_clause"))
                    .or_else(|| node.child_by_field_name( "type_list"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // function foo(): ReturnType
            "function_definition" | "method_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "return_type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Kotlin type binding extraction.
    /// Handles: property_declaration, parameter, function_declaration (return type).
    fn extract_kotlin_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        // Kotlin tree-sitter uses positional children, not named fields.
        // From AST: class_parameter → [binding_pattern_kind, simple_identifier, ":", user_type]
        // From AST: parameter → [simple_identifier, ":", user_type]
        // From AST: function_declaration → [simple_identifier, function_value_parameters, ":", nullable_type|user_type]
        // From AST: property_declaration → [binding_pattern_kind, simple_identifier, ":", user_type]
        let children: Vec<Node> = node.children(&mut node.walk()).collect();
        match node_kind {
            "class_parameter" | "parameter" | "property_declaration" => {
                // Name is the first simple_identifier child
                *var_name = children.iter().find(|c| c.kind() == "simple_identifier")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                // Type is the user_type or nullable_type child
                *type_text = children.iter().find(|c| c.kind() == "user_type" || c.kind() == "nullable_type")
                    .map(|n| {
                        // For nullable_type, extract the inner type_identifier
                        if n.kind() == "nullable_type" {
                            n.children(&mut n.walk())
                                .find(|c| c.kind() == "type_identifier")
                                .map(|c| content[c.start_byte()..c.end_byte()].to_string())
                                .unwrap_or_else(|| content[n.start_byte()..n.end_byte()].to_string())
                        } else {
                            // user_type → type_identifier
                            n.children(&mut n.walk())
                                .find(|c| c.kind() == "type_identifier")
                                .map(|c| content[c.start_byte()..c.end_byte()].to_string())
                                .unwrap_or_else(|| content[n.start_byte()..n.end_byte()].to_string())
                        }
                    });
            }
            "function_declaration" => {
                // Name is the first simple_identifier child
                *var_name = children.iter().find(|c| c.kind() == "simple_identifier")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                // Return type is the nullable_type or user_type after function_value_parameters
                *type_text = children.iter().find(|c| c.kind() == "nullable_type" || c.kind() == "user_type")
                    .map(|n| {
                        n.children(&mut n.walk())
                            .find(|c| c.kind() == "type_identifier")
                            .map(|c| content[c.start_byte()..c.end_byte()].to_string())
                            .unwrap_or_else(|| content[n.start_byte()..n.end_byte()].to_string())
                    });
            }
            _ => {}
        }
    }

    /// Swift type binding extraction.
    /// Handles: property_declaration (pattern_binding), parameter, function_declaration.
    fn extract_swift_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        match node_kind {
            // let x: Type = ...
            "property_declaration" | "class_declaration" | "struct_declaration" => {
                // pattern_binding_list → pattern_binding → pattern → name
                if let Some(pattern) = node.child_by_field_name( "pattern") {
                    *var_name = pattern.child_by_field_name("name")
                        .map(|n| content[n.start_byte()..n.end_byte()].to_string())
                        .or_else(|| {
                            let mut walker = pattern.walk();
                            let mut found = None;
                            let mut cur = walker.node();
                            if walker.goto_first_child() {
                                loop {
                                    if cur.kind() == "simple_identifier" {
                                        found = Some(content[cur.start_byte()..cur.end_byte()].to_string());
                                        break;
                                    }
                                    if !walker.goto_next_sibling() {
                                        break;
                                    }
                                    cur = walker.node();
                                }
                            }
                            found
                        });
                }
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // func foo(param: Type) -> ReturnType
            "parameter" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            "function_declaration" => {
                *var_name = node.child_by_field_name( "name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name( "return_type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Dart type binding extraction.
    /// Handles: property_declaration, parameter, function_declaration, method_signature.
    fn extract_dart_type_binding(
        &self, node: Node, content: &str, node_kind: &str,
        var_name: &mut Option<String>, type_text: &mut Option<String>,
    ) {
        // Dart tree-sitter uses positional children, not named fields.
        // From AST: class_parameter → [binding_pattern_kind, simple_identifier, ":", user_type]
        // From AST: parameter → [simple_identifier, ":", user_type]
        // From AST: initialized_variable_definition → [var, identifier, "=", expression]
        // From AST: function_signature → [return_type?, simple_identifier, formal_parameter_list]
        let children: Vec<Node> = node.children(&mut node.walk()).collect();
        match node_kind {
            // Dart formal_parameter: [type, identifier]
            "formal_parameter" => {
                *var_name = node.child_by_field_name("name")
                    .or_else(|| children.iter().find(|c| c.kind() == "identifier").copied())
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name("type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // Dart function/method signature: return type is a named field
            "function_signature" | "method_signature" => {
                *var_name = node.child_by_field_name("name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name("return_type")
                    .or_else(|| node.child_by_field_name("type"))
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            // Dart class fields: declaration → initialized_identifier_list → initialized_identifier
            "initialized_identifier" | "initialized_variable_definition" => {
                *var_name = node.child_by_field_name("name")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
                *type_text = node.child_by_field_name("type")
                    .map(|n| content[n.start_byte()..n.end_byte()].to_string());
            }
            _ => {}
        }
    }

    /// Dart scope detection.
    fn dart_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" | "enum_declaration" | "extension_declaration"
            | "mixin_declaration" => Some(ScopeKind::Class),
            "function_declaration" | "method_declaration" | "getter_declaration"
            | "setter_declaration" => Some(ScopeKind::Function),
            "constructor_declaration" => Some(ScopeKind::Constructor),
            "function_body" | "block" | "for_statement" | "while_statement"
            | "if_statement" | "switch_statement" | "try_statement"
            | "catch_clause" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    /// Determine if a tree-sitter AST node creates a new scope.
    /// Returns Some(ScopeKind) if it does, None otherwise.
    fn node_scope_kind(&self, node: Node, lang: crate::lang::LanguageId) -> Option<ScopeKind> {
        let kind = node.kind();
        match lang {
            crate::lang::LanguageId::Python => self.python_scope_kind(kind),
            crate::lang::LanguageId::Rust => self.rust_scope_kind(kind),
            crate::lang::LanguageId::TypeScript | crate::lang::LanguageId::JavaScript => {
                self.ts_js_scope_kind(kind)
            }
            crate::lang::LanguageId::Go => self.go_scope_kind(kind),
            crate::lang::LanguageId::Java => self.java_scope_kind(kind),
            crate::lang::LanguageId::C => self.c_scope_kind(kind),
            crate::lang::LanguageId::Cpp => self.cpp_scope_kind(kind),
            crate::lang::LanguageId::CSharp => self.csharp_scope_kind(kind),
            crate::lang::LanguageId::Ruby => self.ruby_scope_kind(kind),
            crate::lang::LanguageId::PHP => self.php_scope_kind(kind),
            crate::lang::LanguageId::Kotlin => self.kotlin_scope_kind(kind),
            crate::lang::LanguageId::Swift => self.swift_scope_kind(kind),
            crate::lang::LanguageId::Dart => self.dart_scope_kind(kind),
            crate::lang::LanguageId::Bash => self.bash_scope_kind(kind),
            crate::lang::LanguageId::JSON | crate::lang::LanguageId::YAML => None,
            crate::lang::LanguageId::Unknown => None,
        }
    }

    fn python_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_definition" => Some(ScopeKind::Class),
            "function_definition" => Some(ScopeKind::Function),
            "lambda" => Some(ScopeKind::Function),
            "list_comprehension" | "set_comprehension" | "dictionary_comprehension"
            | "generator_expression" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn rust_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "struct_item" => Some(ScopeKind::Struct),
            "enum_item" => Some(ScopeKind::Enum),
            "trait_item" => Some(ScopeKind::Trait),
            "impl_item" => Some(ScopeKind::Impl),
            "function_item" => Some(ScopeKind::Function),
            "closure_expression" => Some(ScopeKind::Function),
            "mod_item" => Some(ScopeKind::Module),
            "block" => Some(ScopeKind::Block),
            "for_expression" | "while_expression" | "loop_expression" | "if_expression"
            | "match_expression" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn ts_js_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "interface_declaration" => Some(ScopeKind::Interface),
            "enum_declaration" => Some(ScopeKind::Enum),
            "function_declaration" | "function" => Some(ScopeKind::Function),
            "arrow_function" => Some(ScopeKind::Function),
            "method_definition" => Some(ScopeKind::Method),
            "generator_function_declaration" => Some(ScopeKind::Function),
            "block" => Some(ScopeKind::Block),
            "for_statement" | "for_in_statement" | "for_of_statement"
            | "while_statement" | "do_statement" | "if_statement"
            | "switch_statement" | "try_statement" => Some(ScopeKind::Block),
            "object" => Some(ScopeKind::Block),
            "namespace_declaration" | "module_declaration" => Some(ScopeKind::Namespace),
            _ => None,
        }
    }

    fn go_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "struct_type" => Some(ScopeKind::Struct),
            "interface_type" => Some(ScopeKind::Interface),
            "function_declaration" => Some(ScopeKind::Function),
            "method_declaration" => Some(ScopeKind::Method),
            "func_literal" => Some(ScopeKind::Function),
            "block" => Some(ScopeKind::Block),
            "for_statement" | "if_statement" | "switch_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn java_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "interface_declaration" => Some(ScopeKind::Interface),
            "enum_declaration" => Some(ScopeKind::Enum),
            "record_declaration" => Some(ScopeKind::Struct),
            "method_declaration" => Some(ScopeKind::Method),
            "constructor_declaration" => Some(ScopeKind::Constructor),
            "block" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "switch_statement" | "try_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn c_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "struct_specifier" => Some(ScopeKind::Struct),
            "enum_specifier" => Some(ScopeKind::Enum),
            "union_specifier" => Some(ScopeKind::Struct),
            "function_definition" => Some(ScopeKind::Function),
            "compound_statement" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "switch_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn cpp_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_specifier" => Some(ScopeKind::Class),
            "struct_specifier" => Some(ScopeKind::Struct),
            "enum_specifier" => Some(ScopeKind::Enum),
            "union_specifier" => Some(ScopeKind::Struct),
            "namespace_definition" => Some(ScopeKind::Namespace),
            "function_definition" => Some(ScopeKind::Function),
            "lambda_expression" => Some(ScopeKind::Function),
            "compound_statement" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "switch_statement" | "try_statement" => Some(ScopeKind::Block),
            "template_declaration" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn csharp_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "struct_declaration" => Some(ScopeKind::Struct),
            "interface_declaration" => Some(ScopeKind::Interface),
            "enum_declaration" => Some(ScopeKind::Enum),
            "namespace_declaration" => Some(ScopeKind::Namespace),
            "method_declaration" => Some(ScopeKind::Method),
            "constructor_declaration" => Some(ScopeKind::Constructor),
            "property_declaration" => Some(ScopeKind::Block),
            "block" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "switch_statement" | "try_statement" | "foreach_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn ruby_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class" => Some(ScopeKind::Class),
            "module" => Some(ScopeKind::Module),
            "method" => Some(ScopeKind::Method),
            "singleton_method" => Some(ScopeKind::Method),
            "block" => Some(ScopeKind::Block),
            "do_block" => Some(ScopeKind::Block),
            "lambda" => Some(ScopeKind::Function),
            "for" | "while" | "unless" | "if" | "case" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn php_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "interface_declaration" => Some(ScopeKind::Interface),
            "trait_declaration" => Some(ScopeKind::Trait),
            "enum_declaration" => Some(ScopeKind::Enum),
            "function_definition" => Some(ScopeKind::Function),
            "method_declaration" => Some(ScopeKind::Method),
            "anonymous_function" => Some(ScopeKind::Function),
            "compound_statement" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "switch_statement" | "foreach_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn kotlin_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "interface_declaration" => Some(ScopeKind::Interface),
            "enum_class" => Some(ScopeKind::Enum),
            "object_declaration" => Some(ScopeKind::Class),
            "function_declaration" => Some(ScopeKind::Function),
            "anonymous_function" => Some(ScopeKind::Function),
            "lambda_literal" => Some(ScopeKind::Function),
            "block" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "when_expression" | "try_expression" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn swift_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "class_declaration" => Some(ScopeKind::Class),
            "struct_declaration" => Some(ScopeKind::Struct),
            "enum_declaration" => Some(ScopeKind::Enum),
            "protocol_declaration" => Some(ScopeKind::Interface),
            "function_declaration" => Some(ScopeKind::Function),
            "closure_expression" => Some(ScopeKind::Function),
            "computed_property" => Some(ScopeKind::Block),
            "for_statement" | "while_statement" | "if_statement"
            | "switch_statement" | "do_catch_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }

    fn bash_scope_kind(&self, kind: &str) -> Option<ScopeKind> {
        match kind {
            "function_definition" => Some(ScopeKind::Function),
            "for_statement" | "while_statement" | "if_statement" => Some(ScopeKind::Block),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::{LanguageId, LanguageProvider};
    use tree_sitter::Language;

    // Minimal test provider for Rust
    #[derive(Debug)]
    struct TestRustProvider;
    impl LanguageProvider for TestRustProvider {
        fn id(&self) -> LanguageId { LanguageId::Rust }
        fn extensions(&self) -> &'static [&'static str] { &["rs"] }
        fn tree_sitter_language(&self) -> Language { tree_sitter_rust::LANGUAGE.into() }
        fn query(&self) -> &'static str {
            r#"
(function_item name: (identifier) @name) @definition.function
(call_expression function: (identifier) @call.name) @call
(call_expression function: (field_expression field: (field_identifier) @call.name)) @call
(call_expression function: (scoped_identifier) @call.name) @call
            "#
        }
    }

    #[test]
    fn test_call_form_free() {
        let mut engine = SyntaxEngine::new();
        let provider = TestRustProvider;
        let code = "fn main() { foo(); }";
        let captures = engine.extract_captures(&provider, code);
        let call = captures.iter().find(|c| c.tag == CaptureTag::CallName && c.name == "foo");
        assert!(call.is_some(), "should find foo() call, got: {:?}", captures);
        assert_eq!(call.unwrap().call_form, CallForm::Free, "foo() should be a free call");
    }

    #[test]
    fn test_call_form_member() {
        let mut engine = SyntaxEngine::new();
        let provider = TestRustProvider;
        let code = "fn main() { obj.method(); }";
        let captures = engine.extract_captures(&provider, code);
        let call = captures.iter().find(|c| c.tag == CaptureTag::CallName && c.name == "method");
        assert!(call.is_some(), "should find method() call, got: {:?}", captures);
        assert_eq!(call.unwrap().call_form, CallForm::Member, "obj.method() should be a member call");
        assert_eq!(call.unwrap().receiver, Some("obj".to_string()), "receiver should be obj");
    }

    #[test]
    fn test_call_form_scoped() {
        let mut engine = SyntaxEngine::new();
        let provider = TestRustProvider;
        let code = "fn main() { let x = Foo::new(); }";
        let captures = engine.extract_captures(&provider, code);
        // Foo::new is captured as a single scoped_identifier @call.name
        let call = captures.iter().find(|c| c.tag == CaptureTag::CallName && c.name == "Foo::new");
        assert!(call.is_some(), "should find Foo::new() call, got: {:?}", captures);
        assert_eq!(call.unwrap().call_form, CallForm::Scoped, "Foo::new() should be a scoped call");
    }

    #[test]
    fn test_call_form_self_receiver() {
        let mut engine = SyntaxEngine::new();
        let provider = TestRustProvider;
        let code = "fn main() { self.do_something(); }";
        let captures = engine.extract_captures(&provider, code);
        let call = captures.iter().find(|c| c.tag == CaptureTag::CallName && c.name == "do_something");
        assert!(call.is_some(), "should find self.do_something() call, got: {:?}", captures);
        assert_eq!(call.unwrap().call_form, CallForm::Member, "self.method() should be member");
        assert_eq!(call.unwrap().receiver, Some("self".to_string()), "receiver should be self");
    }
}
