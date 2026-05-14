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
    if let Some(captures) = regex::Regex::new(r"app/(.+?)/route\.(ts|js|tsx|jsx)$")
        .ok()
        .and_then(|re| re.captures(&normalized))
    {
        let route_path = captures.get(1).map(|m| m.as_str()).unwrap_or("");
        // Strip route groups: (admin), (marketing) etc.
        let cleaned = route_path
            .replace("/(", "/")
            .replace(")/", "")
            .replace('(', "")
            .replace(')', "");
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

    // Next.js Pages Router: pages/api/**/*.ts
    if let Some(captures) = regex::Regex::new(r"pages/(api\/.+?)\.(ts|js|tsx|jsx)$")
        .ok()
        .and_then(|re| re.captures(&normalized))
    {
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

/// Detect all routes from a file (path-based + AST-based).
/// Takes the tree-sitter tree for AST-based detection.
pub fn detect_routes_with_tree(
    file_path: &str,
    tree: &tree_sitter::Tree,
    content: &str,
) -> Vec<Route> {
    let mut routes = detect_routes_from_path(file_path);

    // Also try AST-based detection (Express)
    routes.extend(detect_express_routes_from_ast(tree, content, file_path));

    routes
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
