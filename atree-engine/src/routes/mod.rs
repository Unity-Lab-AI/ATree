//! Route Extraction — API endpoint → handler mapping using tree-sitter AST.
//!
//! Detects HTTP route handlers by analyzing the AST, not regex.
//! Frameworks detected:
//! - Express: call_expression where function is member_expression (app.get, router.post, etc.)
//! - Flask/FastAPI: decorator_expression on function definitions
//! - Next.js: file path convention (app/api/**/route.ts, pages/api/**/*.ts)
//!
//! Ported from GitNexus's route-extractors/ but using tree-sitter instead of regex.

use serde::{Serialize, Deserialize};
use std::sync::OnceLock;

static NEXTJS_APP_ROUTER_RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
static NEXTJS_PAGES_ROUTER_RE: OnceLock<Option<regex::Regex>> = OnceLock::new();

fn nextjs_app_router_re() -> &'static Option<regex::Regex> {
    NEXTJS_APP_ROUTER_RE.get_or_init(|| regex::Regex::new(r"app/(.+?)/route\.(ts|js|tsx|jsx)$").ok())
}

fn nextjs_pages_router_re() -> &'static Option<regex::Regex> {
    NEXTJS_PAGES_ROUTER_RE.get_or_init(|| regex::Regex::new(r"pages/(api\/.+?)\.(ts|js|tsx|jsx)$").ok())
}

/// A detected API route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// HTTP method (GET, POST, PUT, DELETE, PATCH, etc.)
    pub method: String,
    /// Route path (e.g., "/api/users/[id]")
    pub path: String,
    /// File path where the handler is defined
    pub file_path: String,
    /// Framework that was detected
    pub framework: String,
    /// Line number of the route definition
    pub line: usize,
}

/// Detect routes from file path patterns (Next.js convention-based).
/// This doesn't need tree-sitter — it's purely path-based.
pub fn detect_routes_from_path(file_path: &str) -> Vec<Route> {
    let mut routes = Vec::new();
    let normalized = file_path.replace('\\', "/");

    // Next.js App Router: app/api/**/route.ts
    if let Some(re) = nextjs_app_router_re() {
        if let Some(captures) = re.captures(&normalized) {
            let route_path = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            // Strip route groups: (admin), (marketing) etc.
            let cleaned = route_path
                .replace("/(", "/")
                .replace(")/", "")
                .replace(['(', ')'], "");
            if cleaned.starts_with("api/") || cleaned == "api" {
                routes.push(Route {
                    method: "ANY".to_string(),
                    path: format!("/{}", cleaned),
                    file_path: file_path.to_string(),
                    framework: "nextjs-app".to_string(),
                    line: 0,
                });
            }
        }
    }

    // Next.js Pages Router: pages/api/**/*.ts
    if let Some(re) = nextjs_pages_router_re() {
        if let Some(captures) = re.captures(&normalized) {
            let mut path = format!(
                "/{}",
                captures.get(1).map(|m| m.as_str()).unwrap_or("")
            );
            path = path.replace("/index", "");
            routes.push(Route {
                method: "ANY".to_string(),
                path,
                file_path: file_path.to_string(),
                framework: "nextjs-pages".to_string(),
                line: 0,
            });
        }
    }

    routes
}

/// Detect Express/Node.js routes by walking the tree-sitter AST.
/// Handles: app.METHOD(path, handler), router.METHOD(path, handler)
/// where METHOD is an HTTP verb and path is a string literal.
///
/// This walks the AST directly instead of using tree-sitter queries,
/// giving us access to the full call expression structure including
/// the path string argument.
pub fn detect_express_routes_from_ast(
    tree: &tree_sitter::Tree,
    content: &str,
    file_path: &str,
) -> Vec<Route> {
    let mut routes = Vec::new();
    let http_methods = [
        "get", "post", "put", "delete", "patch", "head", "options", "all",
        "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "ALL",
    ];

    walk_node_for_routes(tree.root_node(), content, file_path, &http_methods, &mut routes);
    routes
}

fn walk_node_for_routes<'a>(
    node: tree_sitter::Node<'a>,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
    routes: &mut Vec<Route>,
) {
    // Look for call_expression nodes
    if node.kind() == "call_expression" {
        if let Some(route) = parse_express_route_call(node, content, file_path, http_methods) {
            routes.push(route);
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_node_for_routes(cursor.node(), content, file_path, http_methods, routes);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn parse_express_route_call(
    call_node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
) -> Option<Route> {
    // The function must be a member_expression: app.METHOD or router.METHOD
    let func = call_node.child_by_field_name("function")?;
    if func.kind() != "member_expression" {
        return None;
    }

    // Extract the object (app/router) and method (get/post/etc.)
    let obj = func.child_by_field_name("object")?;
    let obj_text = &content[obj.start_byte()..obj.end_byte()];
    if obj_text != "app" && obj_text != "router" {
        return None;
    }

    let method_node = func.child_by_field_name("property")?;
    let method_text = &content[method_node.start_byte()..method_node.end_byte()];
    if !http_methods.contains(&method_text) {
        return None;
    }

    // Extract the path from the first string argument
    let args = call_node.child_by_field_name("arguments")?;
    let path = extract_first_string_arg(args, content)?;

    Some(Route {
        method: method_text.to_uppercase(),
        path,
        file_path: file_path.to_string(),
        framework: "express".to_string(),
        line: call_node.start_position().row,
    })
}

/// Extract the string value from the first string argument node.
fn extract_first_string_arg(args_node: tree_sitter::Node, content: &str) -> Option<String> {
    let mut cursor = args_node.walk();
    if cursor.goto_first_child() {
        loop {
            let node = cursor.node();
            // String literals can be "string" (JS/TS) or "string_content" inside "string"
            if node.kind() == "string" || node.kind() == "string_content" {
                let text = &content[node.start_byte()..node.end_byte()];
                // Strip quotes
                let cleaned = text.trim_matches(|c| c == '\'' || c == '"' || c == '`');
                return Some(cleaned.to_string());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Detect Flask/FastAPI routes from Python AST.
/// Handles: @app.route("/path"), @app.get("/path"), @router.post("/path"), etc.
pub fn detect_flask_routes_from_ast(
    tree: &tree_sitter::Tree,
    content: &str,
    file_path: &str,
) -> Vec<Route> {
    let mut routes = Vec::new();
    let http_methods = [
        "route", "get", "post", "put", "delete", "patch", "head", "options",
    ];

    walk_python_routes(tree.root_node(), content, file_path, &http_methods, &mut routes);
    routes
}

fn walk_python_routes(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
    routes: &mut Vec<Route>,
) {
    if node.kind() == "decorated_definition" {
        // Look for decorators on function definitions
        for i in 0..node.child_count() {
            let child = node.child(i as u32)
                .expect("child index must be valid (checked by child_count)");
            if child.kind() == "decorator" {
                if let Some(route) = parse_python_route_decorator(child, content, file_path, http_methods, node) {
                    routes.push(route);
                }
            }
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_python_routes(cursor.node(), content, file_path, http_methods, routes);
            if !cursor.goto_next_sibling() { break; }
        }
    }
}

fn parse_python_route_decorator(
    decorator: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
    decorated_node: tree_sitter::Node,
) -> Option<Route> {
    // Decorator format: @app.get("/path") or @router.post("/path", methods=["GET"])
    let mut cursor = decorator.walk();
    cursor.goto_first_child(); // skip @
    let call = cursor.node();
    if call.kind() != "call" { return None; }

    let func = call.child_by_field_name("function")?;
    if func.kind() != "attribute" { return None; }

    // Extract object (app/router) and method (get/post/etc.)
    let obj = func.child_by_field_name("object")?;
    let obj_text = &content[obj.start_byte()..obj.end_byte()];
    if !obj_text.contains("app") && !obj_text.contains("router") && !obj_text.contains("blueprint") {
        return None;
    }

    let method_node = func.child_by_field_name("attribute")?;
    let method_text = &content[method_node.start_byte()..method_node.end_byte()];
    if !http_methods.contains(&method_text) { return None; }

    // Extract path from first string argument
    let args = call.child_by_field_name("arguments")?;
    let path = extract_python_string_arg(args, content)?;

    // Get the function name being decorated
    let _func_name = find_decorated_function_name(decorated_node, content).unwrap_or_default();

    let method = if method_text == "route" {
        "ANY".to_string()
    } else {
        method_text.to_uppercase()
    };

    Some(Route {
        method,
        path,
        file_path: file_path.to_string(),
        framework: "flask".to_string(),
        line: decorator.start_position().row,
    })
}

fn extract_python_string_arg(args_node: tree_sitter::Node, content: &str) -> Option<String> {
    let mut cursor = args_node.walk();
    if cursor.goto_first_child() {
        loop {
            let node = cursor.node();
            if node.kind() == "string" {
                let text = &content[node.start_byte()..node.end_byte()];
                // Strip quotes (Python can have various quote styles)
                let cleaned = text.trim_start_matches(['"', '\'', 'f', 'r'])
                    .trim_end_matches(['"', '\'']);
                return Some(cleaned.to_string());
            }
            if !cursor.goto_next_sibling() { break; }
        }
    }
    None
}

fn find_decorated_function_name(node: tree_sitter::Node, content: &str) -> Option<String> {
    for i in 0..node.child_count() {
        let child = node.child(i as u32).unwrap();
        if child.kind() == "function_definition" {
            if let Some(name_node) = child.child_by_field_name("name") {
                return Some(content[name_node.start_byte()..name_node.end_byte()].to_string());
            }
        }
    }
    None
}

/// Detect Rust Axum/Actix-web routes from AST.
/// Handles: .route("/path", get(handler)), .route("/path", post(handler))
/// and #[get("/path")], #[post("/path")] macros
pub fn detect_rust_routes_from_ast(
    tree: &tree_sitter::Tree,
    content: &str,
    file_path: &str,
) -> Vec<Route> {
    let mut routes = Vec::new();
    let http_methods = ["get", "post", "put", "delete", "patch", "head", "options"];

    walk_rust_routes(tree.root_node(), content, file_path, &http_methods, &mut routes);
    routes
}

fn walk_rust_routes(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
    routes: &mut Vec<Route>,
) {
    // Axum: .route("/path", get(handler))
    if node.kind() == "call_expression" {
        if let Some(route) = parse_axum_route_call(node, content, file_path) {
            routes.push(route);
        }
    }
    // Actix: #[get("/path")] or #[post("/path")]
    if node.kind() == "attribute_item" {
        if let Some(route) = parse_actix_route_attr(node, content, file_path, http_methods) {
            routes.push(route);
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_rust_routes(cursor.node(), content, file_path, http_methods, routes);
            if !cursor.goto_next_sibling() { break; }
        }
    }
}

fn parse_axum_route_call(
    call_node: tree_sitter::Node,
    content: &str,
    file_path: &str,
) -> Option<Route> {
    // .route("/path", get(handler))
    let func = call_node.child_by_field_name("function")?;
    if func.kind() != "field_access" { return None; }
    let method_node = func.child_by_field_name("field")?;
    let method_text = &content[method_node.start_byte()..method_node.end_byte()];
    if method_text != "route" { return None; }

    let args = call_node.child_by_field_name("arguments")?;
    // First arg is the path string
    let path = extract_rust_string_arg(args, content)?;

    // Second arg is the method handler: get(handler), post(handler), etc.
    let method = extract_rust_route_method(args, content);

    Some(Route {
        method,
        path,
        file_path: file_path.to_string(),
        framework: "axum".to_string(),
        line: call_node.start_position().row,
    })
}

fn extract_rust_route_method(args_node: tree_sitter::Node, content: &str) -> String {
    let mut cursor = args_node.walk();
    if cursor.goto_first_child() {
        let mut count = 0;
        loop {
            let node = cursor.node();
            count += 1;
            if count >= 3 { // skip "(", first arg, ","
                if node.kind() == "call_expression" {
                    if let Some(func) = node.child_by_field_name("function") {
                        let name = &content[func.start_byte()..func.end_byte()];
                        return name.to_uppercase();
                    }
                }
                // Also handle identifier: get, post, etc.
                if node.kind() == "identifier" {
                    return content[node.start_byte()..node.end_byte()].to_uppercase();
                }
                break;
            }
            if !cursor.goto_next_sibling() { break; }
        }
    }
    "ANY".to_string()
}

fn extract_rust_string_arg(args_node: tree_sitter::Node, content: &str) -> Option<String> {
    let mut cursor = args_node.walk();
    if cursor.goto_first_child() {
        let mut count = 0;
        loop {
            let node = cursor.node();
            count += 1;
            if count >= 2 { // skip "("
                if node.kind() == "string_literal" || node.kind() == "raw_string_literal" {
                    let text = &content[node.start_byte()..node.end_byte()];
                    let cleaned = text.trim_start_matches('"').trim_start_matches('r').trim_start_matches('#')
                        .trim_end_matches('"').trim_end_matches('#');
                    return Some(cleaned.to_string());
                }
                break;
            }
            if !cursor.goto_next_sibling() { break; }
        }
    }
    None
}

fn parse_actix_route_attr(
    attr_node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
) -> Option<Route> {
    // #[get("/path")] or #[post("/path")]
    let attr_name_node = attr_node.child_by_field_name("name")
        .or_else(|| {
            let mut cursor = attr_node.walk();
            cursor.goto_first_child();
            Some(cursor.node())
        })?;
    let attr_name = &content[attr_name_node.start_byte()..attr_name_node.end_byte()];
    if !http_methods.contains(&attr_name) { return None; }

    // Extract path from arguments
    let args = attr_node.child_by_field_name("arguments")?;
    let path = extract_rust_string_arg(args, content)?;

    Some(Route {
        method: attr_name.to_uppercase(),
        path,
        file_path: file_path.to_string(),
        framework: "actix-web".to_string(),
        line: attr_node.start_position().row,
    })
}

/// Detect Rails routes from Ruby AST.
/// Handles: get "/path", post "/path", resources :name, etc.
pub fn detect_rails_routes_from_ast(
    tree: &tree_sitter::Tree,
    content: &str,
    file_path: &str,
) -> Vec<Route> {
    let mut routes = Vec::new();
    let http_methods = ["get", "post", "put", "delete", "patch", "resources", "resource"];

    walk_rails_routes(tree.root_node(), content, file_path, &http_methods, &mut routes);
    routes
}

fn walk_rails_routes(
    node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
    routes: &mut Vec<Route>,
) {
    if node.kind() == "call" {
        // Ruby: get "/path" or resources :name
        if let Some(route) = parse_rails_route_call(node, content, file_path, http_methods) {
            routes.push(route);
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk_rails_routes(cursor.node(), content, file_path, http_methods, routes);
            if !cursor.goto_next_sibling() { break; }
        }
    }
}

fn parse_rails_route_call(
    call_node: tree_sitter::Node,
    content: &str,
    file_path: &str,
    http_methods: &[&str],
) -> Option<Route> {
    // In Ruby tree-sitter, method calls like `get "/path"` are call nodes
    // with method name and argument
    let method_node = call_node.child_by_field_name("method")?;
    let method_text = &content[method_node.start_byte()..method_node.end_byte()];
    if !http_methods.contains(&method_text) { return None; }

    let args = call_node.child_by_field_name("arguments")?;
    let path = extract_rails_string_arg(args, content)?;

    let method = if method_text == "resources" || method_text == "resource" {
        "REST".to_string()
    } else {
        method_text.to_uppercase()
    };

    Some(Route {
        method,
        path,
        file_path: file_path.to_string(),
        framework: "rails".to_string(),
        line: call_node.start_position().row,
    })
}

fn extract_rails_string_arg(args_node: tree_sitter::Node, content: &str) -> Option<String> {
    let mut cursor = args_node.walk();
    if cursor.goto_first_child() {
        loop {
            let node = cursor.node();
            if node.kind() == "string" || node.kind() == "string_literal" || node.kind() == "symbol" {
                let text = &content[node.start_byte()..node.end_byte()];
                let cleaned = text.trim_matches(|c| c == '"' || c == '\'' || c == ':');
                return Some(cleaned.to_string());
            }
            if !cursor.goto_next_sibling() { break; }
        }
    }
    None
}

/// Detect all routes from a file (path-based + AST-based).
/// Takes the tree-sitter tree for AST-based detection.
pub fn detect_routes_with_tree(
    file_path: &str,
    tree: &tree_sitter::Tree,
    content: &str,
) -> Vec<Route> {
    let mut routes = detect_routes_from_path(file_path);

    // Express (JS/TS)
    routes.extend(detect_express_routes_from_ast(tree, content, file_path));

    // Flask/FastAPI (Python)
    routes.extend(detect_flask_routes_from_ast(tree, content, file_path));

    // Axum/Actix-web (Rust)
    routes.extend(detect_rust_routes_from_ast(tree, content, file_path));

    // Rails (Ruby)
    routes.extend(detect_rails_routes_from_ast(tree, content, file_path));

    routes
}

/// Detect routes from already-extracted ParsedFile data (decorators + calls).
/// This is used when the tree-sitter tree is no longer available but we have
/// the extracted semantic data. Works with the existing data model.
pub fn detect_routes_from_parsed(
    file_path: &str,
    language: &str,
    decorators: &[crate::semantic::Decorator],
    calls: &[crate::semantic::Call],
) -> Vec<Route> {
    let mut routes = detect_routes_from_path(file_path);

    match language {
        "python" => {
            // Flask/FastAPI: look for decorators with @app.get, @router.post, etc.
            // The decorator name is stored as the full text like "app.get('/users')"
            for dec in decorators {
                let dec_text = &dec.name;
                if dec_text.contains('.') {
                    let parts: Vec<&str> = dec_text.splitn(2, '.').collect();
                    if parts.len() >= 2 {
                        let obj = parts[0];
                        let rest = parts[1]; // e.g., "get('/users')"
                        if obj.contains("app") || obj.contains("router") || obj.contains("blueprint") {
                            // Extract method and path from the rest
                            if let Some(paren_idx) = rest.find('(') {
                                let method = &rest[..paren_idx];
                                let args = &rest[paren_idx+1..];
                                let http_methods = ["get", "post", "put", "delete", "patch", "head", "options", "route"];
                                if http_methods.contains(&method) {
                                    // Extract path from first string argument
                                    let path = extract_path_from_args(args);
                                    let method_str = if method == "route" { "ANY" } else { method };
                                    routes.push(Route {
                                        method: method_str.to_uppercase(),
                                        path,
                                        file_path: file_path.to_string(),
                                        framework: "flask".to_string(),
                                        line: dec.line,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        "rust" => {
            // Axum: look for .route("...", get(...)) patterns in calls
            for call in calls {
                if call.callee_name == "route" {
                    if let Some(ref receiver) = call.receiver {
                        if receiver.contains("Router") || receiver.contains("App") || receiver.contains("Scope") {
                            routes.push(Route {
                                method: "ANY".to_string(),
                                path: format!("<axum:{}>", receiver),
                                file_path: file_path.to_string(),
                                framework: "axum".to_string(),
                                line: call.line,
                            });
                        }
                    }
                }
            }
        }
        "ruby" => {
            // Rails: look for get "/path", post "/path" in calls
            for call in calls {
                let http_methods = ["get", "post", "put", "delete", "patch", "resources", "resource"];
                if http_methods.contains(&call.callee_name.as_str()) {
                    let method = if call.callee_name == "resources" || call.callee_name == "resource" {
                        "REST".to_string()
                    } else {
                        call.callee_name.to_uppercase()
                    };
                    routes.push(Route {
                        method,
                        path: format!("<rails:{}>", call.callee_name),
                        file_path: file_path.to_string(),
                        framework: "rails".to_string(),
                        line: call.line,
                    });
                }
            }
        }
        _ => {}
    }

    routes
}

/// Extract a path string from decorator/call arguments text.
fn extract_path_from_args(args: &str) -> String {
    // Find the first quoted string in the args
    let chars = args.chars();
    let mut in_string = false;
    let mut quote_char = '"';
    let mut path = String::new();

    for c in chars {
        if !in_string && (c == '"' || c == '\'') {
            in_string = true;
            quote_char = c;
        } else if in_string && c == quote_char {
            break;
        } else if in_string {
            path.push(c);
        }
    }

    if path.is_empty() {
        "<dynamic>".to_string()
    } else {
        path
    }
}

/// Legacy: detect routes from captures only (no path extraction).
/// Keeps backward compatibility with existing tests.
pub fn detect_routes(
    file_path: &str,
    captures: &[crate::syntax::RawCapture],
) -> Vec<Route> {
    let mut routes = detect_routes_from_path(file_path);

    // If no path-based routes found, try AST-based detection from captures
    // (limited: can detect method but not path)
    if routes.is_empty() {
        let http_methods = [
            "get", "post", "put", "delete", "patch", "head", "options", "all",
            "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "ALL",
        ];
        for capture in captures {
            if capture.tag != crate::lang::CaptureTag::CallName {
                continue;
            }
            let method = &capture.name;
            if !http_methods.contains(&method.as_str()) {
                continue;
            }
            if let Some(ref receiver) = capture.receiver {
                if receiver == "app" || receiver == "router" {
                    routes.push(Route {
                        method: method.to_uppercase(),
                        path: String::new(), // Can't extract path from captures alone
                        file_path: file_path.to_string(),
                        framework: "express".to_string(),
                        line: capture.range.start_point.row,
                    });
                }
            }
        }
    }

    routes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nextjs_app_router() {
        let routes = detect_routes_from_path("app/api/users/route.ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/api/users");
        assert_eq!(routes[0].framework, "nextjs-app");
    }

    #[test]
    fn test_nextjs_app_router_dynamic() {
        let routes = detect_routes_from_path("app/api/users/[id]/route.ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/api/users/[id]");
    }

    #[test]
    fn test_nextjs_pages_router() {
        let routes = detect_routes_from_path("pages/api/users.ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/api/users");
        assert_eq!(routes[0].framework, "nextjs-pages");
    }

    #[test]
    fn test_nextjs_pages_router_index() {
        let routes = detect_routes_from_path("pages/api/index.ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path, "/api");
    }

    #[test]
    fn test_non_route_file() {
        let routes = detect_routes_from_path("src/utils/helpers.ts");
        assert!(routes.is_empty());
    }

    #[test]
    fn test_express_ast_detection() {
        // Test with real tree-sitter parsing
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();

        let code = r#"
import express from 'express';
const app = express();
const router = express.Router();

app.get('/users', (req, res) => {});
app.post('/users', (req, res) => {});
app.get('/users/:id', (req, res) => {});
router.put('/users/:id', (req, res) => {});
app.delete('/users/:id', handler);
"#;

        let tree = parser.parse(code, None).unwrap();
        let routes = detect_express_routes_from_ast(&tree, code, "src/routes.ts");

        assert_eq!(routes.len(), 5, "Should find 5 Express routes");

        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].framework, "express");

        assert_eq!(routes[1].method, "POST");
        assert_eq!(routes[1].path, "/users");

        assert_eq!(routes[2].method, "GET");
        assert_eq!(routes[2].path, "/users/:id");

        assert_eq!(routes[3].method, "PUT");
        assert_eq!(routes[3].path, "/users/:id");
        assert_eq!(routes[3].framework, "express");

        assert_eq!(routes[4].method, "DELETE");
        assert_eq!(routes[4].path, "/users/:id");
    }

    #[test]
    fn test_express_non_app_call_ignored() {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();

        let code = r#"
const client = http.createClient();
client.get('/data');
"#;

        let tree = parser.parse(code, None).unwrap();
        let routes = detect_express_routes_from_ast(&tree, code, "src/api.ts");
        assert!(routes.is_empty(), "Non-app calls should not be detected as routes");
    }


}
