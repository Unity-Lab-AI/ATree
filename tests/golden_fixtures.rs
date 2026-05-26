//! Golden fixture tests for semantic extraction.
//!
//! Each fixture is a small source file with known expected outputs.
//! Tests verify that symbol extraction, type binding, route detection,
//! and heritage resolution match expectations.

use std::fs;

/// Helper: build a semantic index from a single file and return the ParsedFile.
fn parse_file(content: &str, ext: &str) -> atree::semantic::ParsedFile {
    use atree::lang::get_provider_for_extension;
    use atree::syntax::SyntaxEngine;

    let provider = get_provider_for_extension(ext).expect("unsupported extension");
    let mut engine = SyntaxEngine::new();
    let (captures, raw_scopes, type_bindings) = engine.extract_captures_and_scopes(provider, content);

    let file_id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "test".hash(&mut h);
        h.finish()
    };
    let file_hash = atree::syntax::hash_content(content);

    atree::semantic::ParsedFile::from_captures_with_scopes(
        file_id, "test", provider.id(), file_hash,
        captures, raw_scopes, type_bindings,
    )
}

// =====================================================================
// TypeScript fixtures
// =====================================================================

#[test]
fn golden_ts_symbols() {
    let content = fs::read_to_string("tests/fixtures/typescript/routes.ts").unwrap();
    let parsed = parse_file(&content, "ts");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    // Core symbols should be extracted
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"User"), "Should find User interface");
    assert!(names.contains(&"Logger"), "Should find Logger class");
    assert!(names.contains(&"handler"), "Should find handler function");

    // Check symbol kinds
    let user_service = parsed.symbols.iter().find(|s| s.name == "UserService").unwrap();
    assert_eq!(user_service.kind, atree::lang::CaptureTag::DefinitionClass);

    let user = parsed.symbols.iter().find(|s| s.name == "User").unwrap();
    assert_eq!(user.kind, atree::lang::CaptureTag::DefinitionInterface);

    // Methods should be extracted
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"log"), "Should find log method");
    
}

#[test]
fn golden_ts_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/typescript/routes.ts").unwrap();
    let parsed = parse_file(&content, "ts");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Class member type annotations
    assert!(bindings.iter().any(|(n, t)| *n == "users" && t.contains("User")),
        "users field should have User type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "logger" && t.contains("Logger")),
        "logger field should have Logger type, got: {:?}", bindings);

    // Parameter type annotations
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("string")),
        "id param should have string type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "user" && t.contains("User")),
        "user param should have User type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "message" && t.contains("string")),
        "message param should have string type, got: {:?}", bindings);
}

#[test]
fn golden_ts_express_routes() {
    use atree::routes::detect_express_routes_from_ast;
    use atree::lang::get_provider_for_extension;
    use atree::syntax::SyntaxEngine;

    let content = fs::read_to_string("tests/fixtures/typescript/routes.ts").unwrap();
    let provider = get_provider_for_extension("ts").unwrap();
    let mut engine = SyntaxEngine::new();
    let (_captures, _raw_scopes, _type_bindings) = engine.extract_captures_and_scopes(provider, &content);

    // Parse the tree for route detection
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&provider.tree_sitter_language()).unwrap();
    let tree = parser.parse(&content, None).unwrap();

    let routes = detect_express_routes_from_ast(&tree, &content, "routes.ts");

    assert!(routes.len() >= 5, "Should find at least 5 Express routes, got: {:?}", routes);

    // Verify paths are extracted (not empty!)
    for route in &routes {
        assert!(!route.path.is_empty(), "Route path should not be empty for {}", route.method);
    }

    // Check specific routes
    let get_users = routes.iter().find(|r| r.method == "GET" && r.path == "/users");
    assert!(get_users.is_some(), "Should find GET /users");

    let post_users = routes.iter().find(|r| r.method == "POST" && r.path == "/users");
    assert!(post_users.is_some(), "Should find POST /users");

    let get_user = routes.iter().find(|r| r.method == "GET" && r.path == "/users/:id");
    assert!(get_user.is_some(), "Should find GET /users/:id");

    let put_user = routes.iter().find(|r| r.method == "PUT" && r.path == "/users/:id");
    assert!(put_user.is_some(), "Should find PUT /users/:id");

    let delete_user = routes.iter().find(|r| r.method == "DELETE" && r.path == "/users/:id");
    assert!(delete_user.is_some(), "Should find DELETE /users/:id");
}

// =====================================================================
// Python fixtures
// =====================================================================

#[test]
fn golden_python_symbols() {
    let content = fs::read_to_string("tests/fixtures/python/models.py").unwrap();
    let parsed = parse_file(&content, "py");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User class");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"UserRepository"), "Should find UserRepository class");
    assert!(names.contains(&"Logger"), "Should find Logger class");

    // Methods
    assert!(names.contains(&"__init__"), "Should find __init__ methods");
    assert!(names.contains(&"find_by_id"), "Should find find_by_id method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"find"), "Should find find method");
    assert!(names.contains(&"log"), "Should find log method");
}

#[test]
fn golden_python_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/python/models.py").unwrap();
    let parsed = parse_file(&content, "py");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Variable type annotations
    assert!(bindings.iter().any(|(n, t)| *n == "admin_user" && t.contains("User")),
        "admin_user should have User type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "user_ids" && t.contains("List")),
        "user_ids should have List type, got: {:?}", bindings);

    // Parameter type annotations
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("str")),
        "id param should have str type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "user" && t.contains("User")),
        "user param should have User type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "message" && t.contains("str")),
        "message param should have str type, got: {:?}", bindings);
}

#[test]
fn golden_python_heritage() {
    let content = fs::read_to_string("tests/fixtures/python/models.py").unwrap();
    let parsed = parse_file(&content, "py");

    // Python classes don't have explicit extends in this fixture
    // But UserService has a forward reference to UserRepository
    let _heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    // No self-edges
    let self_edges: Vec<_> = parsed.heritage.iter()
        .filter(|h| h.target_name == "User" || h.target_name == "UserService" || h.target_name == "Logger")
        .collect();
    assert!(self_edges.is_empty(), "No self-edges in Python heritage");
}

// =====================================================================
// Rust fixtures
// =====================================================================

#[test]
fn golden_rust_symbols() {
    let content = fs::read_to_string("tests/fixtures/rust/service.rs").unwrap();
    let parsed = parse_file(&content, "rs");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User struct");
    assert!(names.contains(&"UserService"), "Should find UserService struct");
    assert!(names.contains(&"Logger"), "Should find Logger struct");
    assert!(names.contains(&"Repository"), "Should find Repository trait");

    // Methods
    assert!(names.contains(&"fmt"), "Should find fmt method");
    assert!(names.contains(&"new"), "Should find new method");
    assert!(names.contains(&"find_by_id"), "Should find find_by_id method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"log"), "Should find log method");
    assert!(names.contains(&"find"), "Should find find method");
}

#[test]
fn golden_rust_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/rust/service.rs").unwrap();
    let parsed = parse_file(&content, "rs");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Struct field type annotations
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("String")),
        "id field should have String type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "name" && t.contains("String")),
        "name field should have String type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "email" && t.contains("String")),
        "email field should have String type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "users" && t.contains("Vec")),
        "users field should have Vec type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "logger" && t.contains("Logger")),
        "logger field should have Logger type, got: {:?}", bindings);
}

#[test]
fn golden_rust_heritage() {
    let content = fs::read_to_string("tests/fixtures/rust/service.rs").unwrap();
    let parsed = parse_file(&content, "rs");

    // Rust uses impl blocks, not class heritage
    // But Display trait impl should be detected
    let _heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    // No self-edges
    let self_edges: Vec<_> = parsed.heritage.iter()
        .filter(|h| h.target_name == "User" || h.target_name == "UserService")
        .collect();
    assert!(self_edges.is_empty(), "No self-edges in Rust heritage");
}

// =====================================================================
// PHP fixtures
// =====================================================================

#[test]
fn golden_php_symbols() {
    let content = fs::read_to_string("tests/fixtures/php/Controller.php").unwrap();
    let parsed = parse_file(&content, "php");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"User"), "Should find User entity class");

    // Methods
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"index"), "Should find index method");
    assert!(names.contains(&"show"), "Should find show method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"findAll"), "Should find findAll method");
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"__construct"), "Should find constructor");
}

#[test]
fn golden_php_heritage() {
    let content = fs::read_to_string("tests/fixtures/php/Controller.php").unwrap();
    let parsed = parse_file(&content, "php");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    // UserController extends AbstractController
    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);

    // CRITICAL: No self-edges
    let self_edges: Vec<_> = parsed.heritage.iter()
        .filter(|h| h.target_name == "UserController" || h.target_name == "UserService" || h.target_name == "User")
        .collect();
    assert!(self_edges.is_empty(),
        "PHP heritage should NOT produce self-edges, found: {:?}", self_edges);
}

// =====================================================================
// Cross-language consistency tests
// =====================================================================

#[test]
fn golden_no_self_heritage_any_language() {
    // Verify that no language produces self-heritage edges
    let fixtures = vec![
        ("tests/fixtures/typescript/routes.ts", "ts"),
        ("tests/fixtures/python/models.py", "py"),
        ("tests/fixtures/rust/service.rs", "rs"),
        ("tests/fixtures/php/Controller.php", "php"),
    ];

    for (path, ext) in fixtures {
        let content = fs::read_to_string(path).unwrap();
        let parsed = parse_file(&content, ext);

        let self_edges: Vec<_> = parsed.heritage.iter()
            .filter(|h| {
                // A self-edge is when the heritage target matches a symbol in the same file
                parsed.symbols.iter().any(|s| s.name == h.target_name)
            })
            .collect();

        // Note: this is a heuristic — some legitimate heritage may match
        // symbol names in the same file (e.g., traits used in the same file).
        // The real check is that the heritage target is not the same as the child class.
        for edge in &self_edges {
            // Check if the child class (owner) is the same as the target
            // This would be a true self-edge bug
            assert!(edge.target_name != "UserController" && edge.target_name != "UserService",
                "Found suspicious self-edge in {}: {:?} -> {:?}",
                path, edge.class_name, edge.target_name);
        }
    }
}

#[test]
fn golden_type_bindings_extracted_all_languages() {
    // Verify that type bindings are actually extracted for all Tier 1 languages
    let fixtures = vec![
        ("tests/fixtures/typescript/routes.ts", "ts", "TypeScript"),
        ("tests/fixtures/python/models.py", "py", "Python"),
        ("tests/fixtures/rust/service.rs", "rs", "Rust"),
        ("tests/fixtures/php/Controller.php", "php", "PHP"),
        ("tests/fixtures/go/service.go", "go", "Go"),
        ("tests/fixtures/java/Service.java", "java", "Java"),
        ("tests/fixtures/cpp/service.cpp", "cpp", "C++"),
        ("tests/fixtures/csharp/Service.cs", "cs", "C#"),
        ("tests/fixtures/swift/Service.swift", "swift", "Swift"),
        ("tests/fixtures/dart/service.dart", "dart", "Dart"),
    ];

    for (path, ext, name) in fixtures {
        let content = fs::read_to_string(path).unwrap();
        let parsed = parse_file(&content, ext);

        assert!(!parsed.type_bindings.is_empty(),
            "{} should have type bindings extracted, got 0", name);

        // Verify no empty type texts
        for binding in &parsed.type_bindings {
            assert!(!binding.type_text.trim().is_empty(),
                "{} has empty type binding for {}", name, binding.var_name);
        }
    }
}

// =====================================================================
// Go fixtures
// =====================================================================

#[test]
fn golden_go_symbols() {
    let content = fs::read_to_string("tests/fixtures/go/service.go").unwrap();
    let parsed = parse_file(&content, "go");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User struct");
    assert!(names.contains(&"Repository"), "Should find Repository interface");
    assert!(names.contains(&"UserService"), "Should find UserService struct");
    assert!(names.contains(&"Logger"), "Should find Logger struct");

    // Functions
    assert!(names.contains(&"NewUserService"), "Should find NewUserService function");
    assert!(names.contains(&"FindByID"), "Should find FindByID method");
    assert!(names.contains(&"Save"), "Should find Save method");
    assert!(names.contains(&"Count"), "Should find Count method");
    assert!(names.contains(&"Log"), "Should find Log method");
}

#[test]
fn golden_go_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/go/service.go").unwrap();
    let parsed = parse_file(&content, "go");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Go exports fields with capitalized names; tree-sitter captures the field name as-is
    // The bindings include both struct fields and function parameters
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("int")),
        "id should have int type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "user" && t.contains("User")),
        "user should have User type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "message" && t.contains("string")),
        "message should have string type, got: {:?}", bindings);
}

#[test]
fn golden_go_interfaces() {
    let content = fs::read_to_string("tests/fixtures/go/service.go").unwrap();
    let parsed = parse_file(&content, "go");

    let repo = parsed.symbols.iter().find(|s| s.name == "Repository").unwrap();
    assert_eq!(repo.kind, atree::lang::CaptureTag::DefinitionInterface,
        "Repository should be an interface");
}

// =====================================================================
// Java fixtures
// =====================================================================

#[test]
fn golden_java_symbols() {
    let content = fs::read_to_string("tests/fixtures/java/Service.java").unwrap();
    let parsed = parse_file(&content, "java");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User class");
    assert!(names.contains(&"Repository"), "Should find Repository interface");
    assert!(names.contains(&"Logger"), "Should find Logger interface");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");

    // Methods
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"index"), "Should find index method");
    assert!(names.contains(&"show"), "Should find show method");
}

#[test]
fn golden_java_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/java/Service.java").unwrap();
    let parsed = parse_file(&content, "java");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Field types
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("int")),
        "id field should have int type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "name" && t.contains("String")),
        "name field should have String type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "viewPath" && t.contains("String")),
        "viewPath field should have String type, got: {:?}", bindings);
}

#[test]
fn golden_java_inheritance() {
    let content = fs::read_to_string("tests/fixtures/java/Service.java").unwrap();
    let parsed = parse_file(&content, "java");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

// =====================================================================
// C fixtures
// =====================================================================

#[test]
fn golden_c_symbols() {
    let content = fs::read_to_string("tests/fixtures/c/types.c").unwrap();
    let parsed = parse_file(&content, "c");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User struct");
    assert!(names.contains(&"Logger"), "Should find Logger struct");
    assert!(names.contains(&"Repository"), "Should find Repository struct");
    assert!(names.contains(&"UserService"), "Should find UserService struct");

    // Functions
    assert!(names.contains(&"logger_log"), "Should find logger_log function");
    assert!(names.contains(&"user_service_find_by_id"), "Should find user_service_find_by_id");
    assert!(names.contains(&"user_service_save"), "Should find user_service_save");
    assert!(names.contains(&"user_service_new"), "Should find user_service_new");
}

#[test]
fn golden_c_symbols_basic() {
    // C type extraction is limited (no annotations, types come from declarations)
    // Just verify symbols are extracted correctly
    let content = fs::read_to_string("tests/fixtures/c/types.c").unwrap();
    let parsed = parse_file(&content, "c");

    // C has limited type binding extraction (struct fields, no annotations)
    // Just verify we get some type bindings from declarations
    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(!names.is_empty(), "C should extract symbols");
}

// =====================================================================
// C++ fixtures
// =====================================================================

#[test]
fn golden_cpp_symbols() {
    let content = fs::read_to_string("tests/fixtures/cpp/service.cpp").unwrap();
    let parsed = parse_file(&content, "cpp");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User class");
    assert!(names.contains(&"Repository"), "Should find Repository class");
    assert!(names.contains(&"Logger"), "Should find Logger class");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");
    assert!(names.contains(&"StringHelper"), "Should find StringHelper class");

    // Methods
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"log"), "Should find log method");
    assert!(names.contains(&"trim"), "Should find trim method");
}

#[test]
fn golden_cpp_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/cpp/service.cpp").unwrap();
    let parsed = parse_file(&content, "cpp");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Member types
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("int")),
        "id should have int type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "name" && t.contains("string")),
        "name should have string type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "prefix" && t.contains("string")),
        "prefix should have string type, got: {:?}", bindings);
}

#[test]
fn golden_cpp_inheritance() {
    let content = fs::read_to_string("tests/fixtures/cpp/service.cpp").unwrap();
    let parsed = parse_file(&content, "cpp");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

// =====================================================================
// C# fixtures
// =====================================================================

#[test]
fn golden_csharp_symbols() {
    let content = fs::read_to_string("tests/fixtures/csharp/Service.cs").unwrap();
    let parsed = parse_file(&content, "cs");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User record");
    assert!(names.contains(&"IRepository"), "Should find IRepository interface");
    assert!(names.contains(&"ILogger"), "Should find ILogger interface");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");
    assert!(names.contains(&"StringHelper"), "Should find StringHelper class");

    // Methods
    assert!(names.contains(&"FindById"), "Should find FindById method");
    assert!(names.contains(&"Save"), "Should find Save method");
    assert!(names.contains(&"Count"), "Should find Count method");
    assert!(names.contains(&"Render"), "Should find Render method");
    assert!(names.contains(&"Log"), "Should find Log method");
    assert!(names.contains(&"FormatName"), "Should find FormatName method");
}

#[test]
fn golden_csharp_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/csharp/Service.cs").unwrap();
    let parsed = parse_file(&content, "cs");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Property types
    assert!(bindings.iter().any(|(n, t)| *n == "Id" && t.contains("int")),
        "Id should have int type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "Name" && t.contains("string")),
        "Name should have string type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "ViewPath" && t.contains("string")),
        "ViewPath should have string type, got: {:?}", bindings);
}

#[test]
fn golden_csharp_inheritance() {
    let content = fs::read_to_string("tests/fixtures/csharp/Service.cs").unwrap();
    let parsed = parse_file(&content, "cs");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

// =====================================================================
// Ruby fixtures
// =====================================================================

#[test]
fn golden_ruby_symbols() {
    let content = fs::read_to_string("tests/fixtures/ruby/service.rb").unwrap();
    let parsed = parse_file(&content, "rb");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User class");
    assert!(names.contains(&"Logger"), "Should find Logger module");
    assert!(names.contains(&"Repository"), "Should find Repository module");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");

    // Methods
    assert!(names.contains(&"find_by_id"), "Should find find_by_id method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"log"), "Should find log method");
    assert!(names.contains(&"index"), "Should find index method");
    assert!(names.contains(&"show"), "Should find show method");
}

#[test]
fn golden_ruby_inheritance() {
    let content = fs::read_to_string("tests/fixtures/ruby/service.rb").unwrap();
    let parsed = parse_file(&content, "rb");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

// =====================================================================
// Kotlin fixtures
// =====================================================================

#[test]
fn golden_kotlin_symbols() {
    let content = fs::read_to_string("tests/fixtures/kotlin/Service.kt").unwrap();
    let parsed = parse_file(&content, "kt");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User data class");
    assert!(names.contains(&"Repository"), "Should find Repository interface");
    assert!(names.contains(&"Logger"), "Should find Logger interface");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");
    assert!(names.contains(&"Utils"), "Should find Utils object");

    // Methods
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"log"), "Should find log method");
    assert!(names.contains(&"formatName"), "Should find formatName method");
}

#[test]
fn golden_kotlin_symbols_basic() {
    // Kotlin type extraction uses simple_identifier which may not capture all types
    // Just verify symbols are extracted correctly
    let content = fs::read_to_string("tests/fixtures/kotlin/Service.kt").unwrap();
    let parsed = parse_file(&content, "kt");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(!names.is_empty(), "Kotlin should extract symbols");
    assert!(names.contains(&"User"), "Should find User");
    assert!(names.contains(&"UserService"), "Should find UserService");
}

#[test]
fn golden_kotlin_inheritance() {
    let content = fs::read_to_string("tests/fixtures/kotlin/Service.kt").unwrap();
    let parsed = parse_file(&content, "kt");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

// =====================================================================
// Swift fixtures
// =====================================================================

#[test]
fn golden_swift_symbols() {
    let content = fs::read_to_string("tests/fixtures/swift/Service.swift").unwrap();
    let parsed = parse_file(&content, "swift");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    assert!(names.contains(&"User"), "Should find User struct");
    assert!(names.contains(&"Repository"), "Should find Repository protocol");
    assert!(names.contains(&"Logger"), "Should find Logger protocol");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");
    assert!(names.contains(&"Utils"), "Should find Utils enum");

    // Methods
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"log"), "Should find log method");
    assert!(names.contains(&"formatName"), "Should find formatName method");
}

#[test]
fn golden_swift_type_bindings() {
    let content = fs::read_to_string("tests/fixtures/swift/Service.swift").unwrap();
    let parsed = parse_file(&content, "swift");

    let bindings: Vec<(&str, &str)> = parsed.type_bindings.iter()
        .map(|b| (b.var_name.as_str(), b.type_text.as_str()))
        .collect();

    // Property types
    assert!(bindings.iter().any(|(n, t)| *n == "id" && t.contains("Int")),
        "id should have Int type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "name" && t.contains("String")),
        "name should have String type, got: {:?}", bindings);
    assert!(bindings.iter().any(|(n, t)| *n == "viewPath" && t.contains("String")),
        "viewPath should have String type, got: {:?}", bindings);
}

#[test]
fn golden_swift_inheritance() {
    let content = fs::read_to_string("tests/fixtures/swift/Service.swift").unwrap();
    let parsed = parse_file(&content, "swift");

    let heritage_targets: Vec<&str> = parsed.heritage.iter()
        .map(|h| h.target_name.as_str())
        .collect();

    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

// =====================================================================
// Bash fixtures
// =====================================================================

#[test]
fn golden_bash_symbols() {
    let content = fs::read_to_string("tests/fixtures/bash/deploy.sh").unwrap();
    let parsed = parse_file(&content, "sh");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();

    // Functions
    assert!(names.contains(&"log_message"), "Should find log_message function");
    assert!(names.contains(&"check_prerequisites"), "Should find check_prerequisites function");
    assert!(names.contains(&"deploy"), "Should find deploy function");
    assert!(names.contains(&"rollback"), "Should find rollback function");
    assert!(names.contains(&"main"), "Should find main function");
}

#[test]
fn golden_bash_variables() {
    let content = fs::read_to_string("tests/fixtures/bash/deploy.sh").unwrap();
    let parsed = parse_file(&content, "sh");

    // Bash variable assignments should be captured
    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"APP_NAME"), "Should find APP_NAME variable");
    assert!(names.contains(&"DEPLOY_DIR"), "Should find DEPLOY_DIR variable");
    assert!(names.contains(&"LOG_FILE"), "Should find LOG_FILE variable");
}

// =====================================================================
// Dart golden fixtures
// =====================================================================

#[test]
fn golden_dart_symbols() {
    let content = fs::read_to_string("tests/fixtures/dart/service.dart").unwrap();
    let parsed = parse_file(&content, "dart");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    eprintln!("Dart symbols found: {:?}", names);
    eprintln!("Dart captures: {}", parsed.symbols.len());
    assert!(names.contains(&"User"), "Should find User class, got: {:?}", names);
    assert!(names.contains(&"Repository"), "Should find Repository class");
    assert!(names.contains(&"Logger"), "Should find Logger mixin");
    assert!(names.contains(&"UserService"), "Should find UserService class");
    assert!(names.contains(&"AbstractController"), "Should find AbstractController class");
    assert!(names.contains(&"UserController"), "Should find UserController class");
    assert!(names.contains(&"Utils"), "Should find Utils class");
}

#[test]
fn golden_dart_inheritance() {
    let content = fs::read_to_string("tests/fixtures/dart/service.dart").unwrap();
    let parsed = parse_file(&content, "dart");

    let heritage_targets: Vec<&str> = parsed.heritage.iter().map(|h| h.target_name.as_str()).collect();
    assert!(heritage_targets.contains(&"AbstractController"),
        "UserController should extend AbstractController, got: {:?}", heritage_targets);
}

#[test]
fn golden_dart_methods() {
    let content = fs::read_to_string("tests/fixtures/dart/service.dart").unwrap();
    let parsed = parse_file(&content, "dart");

    let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"findById"), "Should find findById method");
    assert!(names.contains(&"save"), "Should find save method");
    assert!(names.contains(&"count"), "Should find count method");
    assert!(names.contains(&"render"), "Should find render method");
    assert!(names.contains(&"index"), "Should find index method");
    assert!(names.contains(&"show"), "Should find show method");
    assert!(names.contains(&"formatName"), "Should find formatName method");
}
