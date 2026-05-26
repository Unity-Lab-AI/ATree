//! ORM query extraction — Prisma and Supabase via tree-sitter AST walking.
//!
//! Modeled after GitNexusRelay's:
//! - `gitnexus/src/core/ingestion/pipeline-phases/orm.ts`
//! - `gitnexus/src/core/ingestion/pipeline-phases/orm-extraction.ts`
//!
//! Instead of regex, we parse JS/TS files with tree-sitter and walk the AST
//! to find Prisma (`prisma.model.method()`) and Supabase
//! (`supabase.from('table').method()`) call patterns structurally.
//!
//! Falls back to regex for non-JS/TS files or when parsing fails.

use serde::{Serialize, Deserialize};
use tree_sitter::{Parser, Tree, Node, Language};
use std::sync::OnceLock;

// ── ORM query types ──────────────────────────────────────────────────────────

/// An extracted ORM query call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedOrmQuery {
    /// Relative path of the source file.
    pub file_path: String,
    /// ORM system: "prisma" or "supabase".
    pub orm: String,
    /// Model/table name (e.g., "user", "users").
    pub model: String,
    /// Method called (e.g., "findMany", "select").
    pub method: String,
    /// 0-based line number of the call.
    pub line_number: usize,
}

/// Result of ORM extraction across all files.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrmExtractionResult {
    pub queries: Vec<ExtractedOrmQuery>,
    pub prisma_count: usize,
    pub supabase_count: usize,
}

// ── Language detection ───────────────────────────────────────────────────────

/// Check if a file is JavaScript or TypeScript (tree-sitter parsable for ORM).
fn is_js_ts_file(path: &str) -> bool {
    matches!(
        path.rsplit('.').next(),
        Some("js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs")
    )
}

// ── Tree-sitter AST walking ──────────────────────────────────────────────────

/// Get the tree-sitter language for JS/TS files.
fn ts_language(path: &str) -> Option<Language> {
    let ext = path.rsplit('.').next()?;
    match ext {
        "js" | "jsx" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" | "tsx" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        _ => None,
    }
}

/// Get the text content of a node.
fn node_text<'a>(node: Node<'a>, content: &'a str) -> &'a str {
    let start = node.start_byte();
    let end = node.end_byte();
    &content[start..end]
}

/// Check if a node is an identifier (or property_identifier) with the given text.
fn is_identifier_with(node: Node, text: &str, content: &str) -> bool {
    let kind = node.kind();
    (kind == "identifier" || kind == "property_identifier") && node_text(node, content) == text
}

/// Walk the AST to find Prisma and Supabase calls.
fn walk_for_orm_calls(tree: &Tree, content: &str, file_path: &str, out: &mut Vec<ExtractedOrmQuery>) {
    walk_node(tree.root_node(), content, file_path, out);
}

fn walk_node(node: Node, content: &str, file_path: &str, out: &mut Vec<ExtractedOrmQuery>) {
    // Try to match Prisma pattern: prisma.<model>.<method>(...)
    if let Some(query) = match_prisma_call(node, content, file_path) {
        out.push(query);
    }
    // Try to match Supabase pattern: supabase.from('<model>').<method>(...)
    if let Some(query) = match_supabase_call(node, content, file_path) {
        out.push(query);
    }

    // Recurse into children.
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_node(cursor.node(), content, file_path, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Match: `prisma.<model>.<method>(...)`
///
/// AST shape:
/// call_expression
///   function: member_expression
///     object: member_expression
///       object: identifier "prisma"
///       property: identifier <model>
///     property: identifier <method>
///   arguments: (...)
fn match_prisma_call(node: Node, content: &str, file_path: &str) -> Option<ExtractedOrmQuery> {
    // Must be a call_expression
    if node.kind() != "call_expression" {
        return None;
    }

    // Get the function being called
    let func = node.child_by_field_name("function")?;

    // Must be a member_expression (prisma.something.something)
    if func.kind() != "member_expression" {
        return None;
    }

    // The method is the property of the outer member_expression
    let method_node = func.child_by_field_name("property")?;
    let method_kind = method_node.kind();
    if method_kind != "identifier" && method_kind != "property_identifier" {
        return None;
    }
    let method = node_text(method_node, content).to_string();

    // The object of the outer member_expression should be another member_expression
    let inner = func.child_by_field_name("object")?;
    if inner.kind() != "member_expression" {
        return None;
    }

    // The model is the property of the inner member_expression
    let model_node = inner.child_by_field_name("property")?;
    let model_kind = model_node.kind();
    if model_kind != "identifier" && model_kind != "property_identifier" {
        return None;
    }
    let model = node_text(model_node, content).to_string();

    // The object of the inner member_expression should be "prisma"
    let prisma_node = inner.child_by_field_name("object")?;
    if !is_identifier_with(prisma_node, "prisma", content) {
        return None;
    }

    // Skip internal Prisma models (e.g., $transaction)
    if model.starts_with('$') {
        return None;
    }

    let line_number = node.start_position().row;

    Some(ExtractedOrmQuery {
        file_path: file_path.to_string(),
        orm: "prisma".to_string(),
        model,
        method,
        line_number,
    })
}

/// Match: `supabase.from('<model>').<method>(...)`
///
/// AST shape:
/// call_expression
///   function: member_expression
///     object: call_expression
///       function: member_expression
///         object: identifier "supabase"
///         property: identifier "from"
///       arguments: string_literal <model>
///     property: identifier <method>
///   arguments: (...)
fn match_supabase_call(node: Node, content: &str, file_path: &str) -> Option<ExtractedOrmQuery> {
    // Must be a call_expression
    if node.kind() != "call_expression" {
        return None;
    }

    // Get the function being called
    let func = node.child_by_field_name("function")?;

    // Must be a member_expression (something.<method>)
    if func.kind() != "member_expression" {
        return None;
    }

    // The method is the property
    let method_node = func.child_by_field_name("property")?;
    let method_kind = method_node.kind();
    if method_kind != "identifier" && method_kind != "property_identifier" {
        return None;
    }
    let method = node_text(method_node, content).to_string();

    // The object should be a call_expression (supabase.from('<model>'))
    let from_call = func.child_by_field_name("object")?;
    if from_call.kind() != "call_expression" {
        return None;
    }

    // The function of the from_call should be a member_expression (supabase.from)
    let from_func = from_call.child_by_field_name("function")?;
    if from_func.kind() != "member_expression" {
        return None;
    }

    // The property should be "from"
    let from_prop = from_func.child_by_field_name("property")?;
    if !is_identifier_with(from_prop, "from", content) {
        return None;
    }

    // The object should be "supabase"
    let supabase_node = from_func.child_by_field_name("object")?;
    if !is_identifier_with(supabase_node, "supabase", content) {
        return None;
    }

    // The first argument to .from() should be a string literal (the table name)
    let args = from_call.child_by_field_name("arguments")?;
    let first_arg = args.named_child(0)?;
    let model = if first_arg.kind() == "string" || first_arg.kind() == "string_literal" || first_arg.kind() == "template_string" {
        // Extract the string content (strip quotes)
        let raw = node_text(first_arg, content);
        if raw.len() >= 2 {
            raw[1..raw.len()-1].to_string()
        } else {
            raw.to_string()
        }
    } else if first_arg.kind() == "template_string" {
        // Template string: extract the first content node
        let content_node = first_arg.named_child(0)?;
        node_text(content_node, content).to_string()
    } else {
        return None;
    };

    let line_number = node.start_position().row;

    Some(ExtractedOrmQuery {
        file_path: file_path.to_string(),
        orm: "supabase".to_string(),
        model,
        method,
        line_number,
    })
}

// ── Regex fallback for non-JS/TS files ──────────────────────────────────────

static PRISMA_RE: OnceLock<regex::Regex> = OnceLock::new();
static SUPABASE_RE: OnceLock<regex::Regex> = OnceLock::new();

fn prisma_regex() -> &'static regex::Regex {
    PRISMA_RE.get_or_init(|| {
        regex::Regex::new(r"\bprisma\.(\w+)\.(findMany|findFirst|findUnique|findUniqueOrThrow|findFirstOrThrow|create|createMany|update|updateMany|delete|deleteMany|upsert|count|aggregate|groupBy)\s*\(").unwrap()
    })
}

fn supabase_regex() -> &'static regex::Regex {
    SUPABASE_RE.get_or_init(|| {
        regex::Regex::new(r#"\bsupabase\.from\s*\(\s*['"](\w+)['"]\s*\)\s*\.(select|insert|update|delete|upsert)\s*\("#).unwrap()
    })
}

/// Regex-based extraction fallback for non-JS/TS files.
fn extract_orm_regex(file_path: &str, content: &str, out: &mut Vec<ExtractedOrmQuery>) {
    let has_prisma = content.contains("prisma.");
    let has_supabase = content.contains("supabase") && content.contains(".from(");
    if !has_prisma && !has_supabase {
        return;
    }

    let line_offsets = build_line_offsets(content);

    if has_prisma {
        for cap in prisma_regex().captures_iter(content) {
            let model = cap.get(1).unwrap().as_str();
            if model.starts_with('$') {
                continue;
            }
            let method = cap.get(2).unwrap().as_str();
            let offset = cap.get(0).unwrap().start();
            out.push(ExtractedOrmQuery {
                file_path: file_path.to_string(),
                orm: "prisma".to_string(),
                model: model.to_string(),
                method: method.to_string(),
                line_number: line_number_at_offset(&line_offsets, offset),
            });
        }
    }

    if has_supabase {
        for cap in supabase_regex().captures_iter(content) {
            let model = cap.get(1).unwrap().as_str();
            let method = cap.get(2).unwrap().as_str();
            let offset = cap.get(0).unwrap().start();
            out.push(ExtractedOrmQuery {
                file_path: file_path.to_string(),
                orm: "supabase".to_string(),
                model: model.to_string(),
                method: method.to_string(),
                line_number: line_number_at_offset(&line_offsets, offset),
            });
        }
    }
}

/// Build an array of byte offsets where each newline occurs.
fn build_line_offsets(content: &str) -> Vec<usize> {
    content.char_indices()
        .filter(|(_, c)| *c == '\n')
        .map(|(i, _)| i)
        .collect()
}

/// Binary search for 0-based line number at a given character offset.
fn line_number_at_offset(line_offsets: &[usize], offset: usize) -> usize {
    match line_offsets.binary_search(&offset) {
        Ok(idx) => idx,
        Err(idx) => idx,
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Extract ORM query calls from file content.
///
/// Uses tree-sitter AST walking for JS/TS files (more robust),
/// falls back to regex for other file types.
pub fn extract_orm_queries(file_path: &str, content: &str, out: &mut Vec<ExtractedOrmQuery>) {
    if is_js_ts_file(file_path) {
        if let Some(lang) = ts_language(file_path) {
            let mut parser = Parser::new();
            if parser.set_language(&lang).is_ok() {
                if let Some(tree) = parser.parse(content, None) {
                    walk_for_orm_calls(&tree, content, file_path, out);
                    return;
                }
            }
        }
    }
    // Fallback to regex
    extract_orm_regex(file_path, content, out);
}

/// Extract ORM queries from multiple files.
pub fn extract_orm_queries_from_files(files: &[(String, String)]) -> OrmExtractionResult {
    let mut queries = Vec::new();
    for (path, content) in files {
        extract_orm_queries(path, content, &mut queries);
    }
    let prisma_count = queries.iter().filter(|q| q.orm == "prisma").count();
    let supabase_count = queries.iter().filter(|q| q.orm == "supabase").count();
    OrmExtractionResult { queries, prisma_count, supabase_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prisma_find_many() {
        let content = "\nconst users = await prisma.user.findMany({\n  where: { active: true }\n});\n";
        let mut queries = Vec::new();
        extract_orm_queries(r"src/user.ts", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].orm, "prisma");
        assert_eq!(queries[0].model, "user");
        assert_eq!(queries[0].method, "findMany");
    }

    #[test]
    fn test_prisma_multiple_models() {
        let content = r#"
const users = await prisma.user.findMany();
const posts = await prisma.post.findFirst({ where: { id: 1 } });
const newUser = await prisma.user.create({ data: { name: "Alice" } });
"#;
        let mut queries = Vec::new();
        extract_orm_queries("src/db.ts", content, &mut queries);
        assert_eq!(queries.len(), 3);
        assert_eq!(queries[0].model, "user");
        assert_eq!(queries[0].method, "findMany");
        assert_eq!(queries[1].model, "post");
        assert_eq!(queries[1].method, "findFirst");
        assert_eq!(queries[2].model, "user");
        assert_eq!(queries[2].method, "create");
    }

    #[test]
    fn test_supabase_select() {
        let content = r#"
const { data } = await supabase.from('users').select('*');
"#;
        let mut queries = Vec::new();
        extract_orm_queries("src/api.ts", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].orm, "supabase");
        assert_eq!(queries[0].model, "users");
        assert_eq!(queries[0].method, "select");
    }

    #[test]
    fn test_supabase_insert() {
        let content = r#"
await supabase.from('posts').insert({ title: 'Hello' });
"#;
        let mut queries = Vec::new();
        extract_orm_queries("src/api.ts", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].method, "insert");
    }

    #[test]
    fn test_mixed_prisma_and_supabase() {
        let content = r#"
const users = await prisma.user.findMany();
const { data } = await supabase.from('logs').select('*');
"#;
        let mut queries = Vec::new();
        extract_orm_queries("src/mixed.ts", content, &mut queries);
        assert_eq!(queries.len(), 2);
        assert_eq!(queries[0].orm, "prisma");
        assert_eq!(queries[1].orm, "supabase");
    }

    #[test]
    fn test_skip_internal_prisma_models() {
        let content = r#"
const result = await prisma.$transaction([...]);
const users = await prisma.user.findMany();
"#;
        let mut queries = Vec::new();
        extract_orm_queries("src/db.ts", content, &mut queries);
        assert_eq!(queries.len(), 1); // $transaction skipped
        assert_eq!(queries[0].model, "user");
    }

    #[test]
    fn test_no_orm_content() {
        let content = "const x = 42;\nconsole.log(x);\n";
        let mut queries = Vec::new();
        extract_orm_queries("src/plain.ts", content, &mut queries);
        assert!(queries.is_empty());
    }

    #[test]
    fn test_line_numbers() {
        let content = "line0\nline1\nprisma.user.findMany()\nline3\n";
        let mut queries = Vec::new();
        extract_orm_queries("src/test.ts", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].line_number, 2); // 0-based
    }

    #[test]
    fn test_extract_from_multiple_files() {
        let files = vec![
            ("src/a.ts".to_string(), "prisma.user.findMany()".to_string()),
            ("src/b.ts".to_string(), "supabase.from('posts').select('*')".to_string()),
            ("src/c.ts".to_string(), "const x = 1;".to_string()),
        ];
        let result = extract_orm_queries_from_files(&files);
        assert_eq!(result.queries.len(), 2);
        assert_eq!(result.prisma_count, 1);
        assert_eq!(result.supabase_count, 1);
    }

    #[test]
    fn test_js_file_uses_tree_sitter() {
        let content = "prisma.user.findMany()";
        let mut queries = Vec::new();
        extract_orm_queries(r"src/test.js", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].orm, "prisma");
    }

    #[test]
    fn test_non_js_file_uses_regex() {
        let content = "prisma.user.findMany()";
        let mut queries = Vec::new();
        extract_orm_queries(r"src/test.py", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].orm, "prisma");
    }

    #[test]
    fn test_supabase_double_quotes() {
        let content = r#"await supabase.from("users").select("*")"#;
        let mut queries = Vec::new();
        extract_orm_queries(r"src/api.ts", content, &mut queries);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].model, "users");
    }

    #[test]
    fn test_is_js_ts_file() {
        assert!(is_js_ts_file(r"app.ts"));
        assert!(is_js_ts_file(r"app.tsx"));
        assert!(is_js_ts_file(r"app.js"));
        assert!(is_js_ts_file(r"app.jsx"));
        assert!(is_js_ts_file(r"app.mjs"));
        assert!(is_js_ts_file(r"app.cjs"));
        assert!(!is_js_ts_file(r"app.py"));
        assert!(!is_js_ts_file(r"app.rs"));
    }
}
