use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor, Node, Tree};
use crate::lang::{LanguageProvider, CaptureTag};
use crate::semantic::ScopeKind;

pub struct SyntaxEngine;

/// Compute a fast hash of file content for change detection.
#[inline]
pub fn hash_content(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

pub struct RawCapture {
    pub tag: CaptureTag,
    pub name: String,
    pub range: tree_sitter::Range,
}

/// A scope node extracted from the AST during the walk.
#[derive(Debug, Clone)]
pub struct RawScope {
    pub kind: ScopeKind,
    pub line_start: usize,
    pub line_end: usize,
    pub parent_idx: Option<usize>, // index into the scope stack during extraction
}

impl SyntaxEngine {
    pub fn new() -> Self {
        Self
    }

    pub fn extract_captures(&mut self, provider: &dyn LanguageProvider, content: &str) -> Vec<RawCapture> {
        let mut parser = Parser::new();
        if parser.set_language(&provider.tree_sitter_language()).is_err() {
            return Vec::new();
        }

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => {
                return Vec::new();
            }
        };

        let query = match Query::new(&provider.tree_sitter_language(), provider.query()) {
            Ok(q) => q,
            Err(_) => {
                return Vec::new();
            }
        };

        let capture_names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());

        let mut captures = Vec::new();
        let mut seen = std::collections::HashSet::<(String, usize, usize)>::new();

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
                // Get the name text from the @name capture node
                let name_capture = m
                    .captures
                    .iter()
                    .find(|c| c.index as usize == name_idx)
                    .unwrap();
                let name_text =
                    &content[name_capture.node.start_byte()..name_capture.node.end_byte()];
                let name_range = name_capture.node.range();

                // Pair the name with all semantic tags in this match.
                // Skip wrapper tags (CallWrapper, ImportWrapper, HeritageWrapper) —
                // they're redundant when we have the specific tag (CallName, etc.).
                for &(_, ref tag) in &semantic_captures {
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
                    );
                    if seen.insert(key) {
                        captures.push(RawCapture {
                            tag: *tag,
                            name: name_text.to_string(),
                            range: name_range,
                        });
                    }
                }
            } else {
                // No @name capture — use the capture text directly
                // (import.source, heritage.*, decorator, http_client, assignment)
                for &(idx, ref tag) in &semantic_captures {
                    let c = m.captures.iter().find(|c| c.index as usize == idx).unwrap();
                    let text = &content[c.node.start_byte()..c.node.end_byte()];
                    let key = (text.to_string(), c.node.start_byte(), c.node.end_byte());
                    if seen.insert(key) {
                        captures.push(RawCapture {
                            tag: *tag,
                            name: text.to_string(),
                            range: c.node.range(),
                        });
                    }
                }
            }
        }
        captures
    }

    /// Extract both captures and scope tree from a file.
    /// This is the main entry point for semantic analysis.
    pub fn extract_captures_and_scopes(
        &mut self,
        provider: &dyn LanguageProvider,
        content: &str,
    ) -> (Vec<RawCapture>, Vec<RawScope>) {
        let mut parser = Parser::new();
        if parser.set_language(&provider.tree_sitter_language()).is_err() {
            return (Vec::new(), Vec::new());
        }

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return (Vec::new(), Vec::new()),
        };

        // Extract captures using the query
        let captures = self.extract_captures(provider, content);

        // Walk the AST to extract scope tree
        let scopes = self.extract_scopes_from_tree(&tree, content, provider.id());

        (captures, scopes)
    }

    /// Walk the tree-sitter AST and extract scope-defining nodes.
    /// Returns a flat list of RawScope with parent_idx pointing to the
    /// index of the parent scope in the same list.
    fn extract_scopes_from_tree(
        &self,
        tree: &Tree,
        content: &str,
        lang: crate::lang::LanguageId,
    ) -> Vec<RawScope> {
        let root = tree.root_node();
        let mut scopes: Vec<RawScope> = Vec::new();
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

        self.walk_node(root, content, lang, &mut scopes, &mut scope_stack);

        scopes
    }

    fn walk_node<'a>(
        &self,
        node: Node<'a>,
        content: &str,
        lang: crate::lang::LanguageId,
        scopes: &mut Vec<RawScope>,
        scope_stack: &mut Vec<(Node<'a>, usize)>,
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

        // Recurse into children
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.walk_node(cursor.node(), content, lang, scopes, scope_stack);
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
