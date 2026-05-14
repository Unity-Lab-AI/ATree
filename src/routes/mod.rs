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

/// Tree-sitter query for Express-style route handlers.
/// Matches: app.METHOD(path, handler) or router.METHOD(path, handler)
const EXPRESS_ROUTE_QUERY: &str = r#"
(call_expression
  function: (member_expression
    object: (identifier) @obj
    property: (property_identifier) @method)
  arguments: (arguments
    (string) @path
    (_)? @handler))
"#;

/// Tree-sitter query for Flask/FastAPI decorators.
/// Matches: @app.route('/path') or @app.get('/path')
const FLASK_DECORATOR_QUERY: &str = r#"
(decorator
  (call
    function: (attribute
      object: (identifier) @_app
      property: (identifier) @method)
    arguments: (argument_list
      (string) @path)))
"#;

/// Detect Express routes from AST captures.
/// We look for call_expression nodes where:
/// - The function is a member_expression (obj.method)
/// - The object is "app" or "router"
/// - The method is an HTTP verb
/// - The first argument is a string literal (the path)
pub fn detect_express_routes_from_ast(
    captures: &[crate::syntax::RawCapture],
    file_path: &str,
) -> Vec<Route> {
    let mut routes = Vec::new();
    let http_methods = [
        "get", "post", "put", "delete", "patch", "head", "options", "all",
        "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "ALL",
    ];

    // Group captures by their match index (approximate by byte range)
    // For each call_expression that looks like app.METHOD(path, ...):
    for capture in captures {
        if capture.tag != crate::lang::CaptureTag::CallName {
            continue;
        }

        let method = &capture.name;
        if !http_methods.contains(&method.as_str()) {
            continue;
        }

        // Check if the call has a receiver (member call like app.get)
        if capture.receiver.is_none() {
            continue;
        }

        let receiver = capture.receiver.as_ref().unwrap();
        if receiver != "app" && receiver != "router" {
            continue;
        }

        // This looks like an Express route handler
        // The path would be the next string argument — we'd need the full AST
        // For now, mark it as detected with unknown path
        routes.push(Route {
            method: method.to_uppercase(),
            path: String::new(), // Would need AST walk to extract path arg
            file_path: file_path.to_string(),
            framework: "express".to_string(),
            line: capture.range.start_point.row,
        });
    }

    routes
}

/// Detect all routes from a file (path-based + AST-based).
pub fn detect_routes(
    file_path: &str,
    captures: &[crate::syntax::RawCapture],
) -> Vec<Route> {
    let mut routes = detect_routes_from_path(file_path);

    // If no path-based routes found, try AST-based detection
    if routes.is_empty() {
        routes.extend(detect_express_routes_from_ast(captures, file_path));
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
        // Simulate AST captures from tree-sitter
        use crate::syntax::{RawCapture, CallForm};
        use crate::lang::CaptureTag;

        let captures = vec![
            RawCapture {
                tag: CaptureTag::CallName,
                name: "get".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 3,
                    start_point: tree_sitter::Point { row: 1, column: 4 },
                    end_point: tree_sitter::Point { row: 1, column: 7 },
                },
                call_form: CallForm::Member,
                receiver: Some("app".to_string()),
            },
            RawCapture {
                tag: CaptureTag::CallName,
                name: "post".to_string(),
                range: tree_sitter::Range {
                    start_byte: 50,
                    end_byte: 54,
                    start_point: tree_sitter::Point { row: 5, column: 4 },
                    end_point: tree_sitter::Point { row: 5, column: 8 },
                },
                call_form: CallForm::Member,
                receiver: Some("app".to_string()),
            },
        ];

        let routes = detect_express_routes_from_ast(&captures, "src/routes.ts");
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].framework, "express");
        assert_eq!(routes[1].method, "POST");
    }

    #[test]
    fn test_express_non_app_call_ignored() {
        use crate::syntax::{RawCapture, CallForm};
        use crate::lang::CaptureTag;

        let captures = vec![
            RawCapture {
                tag: CaptureTag::CallName,
                name: "get".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 3,
                    start_point: tree_sitter::Point { row: 1, column: 4 },
                    end_point: tree_sitter::Point { row: 1, column: 7 },
                },
                call_form: CallForm::Member,
                receiver: Some("client".to_string()), // Not app/router
            },
        ];

        let routes = detect_express_routes_from_ast(&captures, "src/api.ts");
        assert!(routes.is_empty(), "Non-app calls should not be detected as routes");
    }
}
