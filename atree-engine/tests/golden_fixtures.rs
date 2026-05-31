use std::fs;

fn parse(content: &str, ext: &str) -> atree_engine::semantic::ParsedFile {
    use atree_engine::lang::get_provider_for_extension;
    use atree_engine::syntax::SyntaxEngine;
    let p = get_provider_for_extension(ext).unwrap();
    let mut e = SyntaxEngine::new();
    let (caps, scopes, tbs) = e.extract_captures_and_scopes(p, content);
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    "t".hash(&mut h);
    atree_engine::semantic::ParsedFile::from_captures_with_scopes(h.finish(), "t", p.id(), atree_engine::syntax::hash_content(content), caps, scopes, tbs)
}

macro_rules! fixture {
    ($path:expr, $ext:expr) => { parse(&fs::read_to_string($path).unwrap(), $ext) };
}

macro_rules! assert_sym {
    ($f:expr, $($sym:expr),+) => {
        let __names: Vec<&str> = $f.symbols.iter().map(|s| s.name.as_str()).collect();
        $(assert!(__names.contains(&$sym), "Expected '{}' not found in {:?}", $sym, __names);)+
    };
}

macro_rules! assert_bind {
    ($f:expr, $($var:expr => $ty:expr),+) => {
        let __b: Vec<(&str, &str)> = $f.type_bindings.iter().map(|b| (b.var_name.as_str(), b.type_text.as_str())).collect();
        $(assert!(__b.iter().any(|(n,t)| *n == $var && t.contains($ty)), "Expected '{}' => '{}' not found in {:?}", $var, $ty, __b);)+
    };
}

macro_rules! assert_herit {
    ($f:expr, $($t:expr),+) => {
        let __h: Vec<&str> = $f.heritage.iter().map(|h| h.target_name.as_str()).collect();
        $(assert!(__h.contains(&$t), "Expected heritage '{}' not found in {:?}", $t, __h);)+
    };
}

#[test] fn golden_ts_symbols() { let f = fixture!("../tests/fixtures/typescript/routes.ts", "ts"); assert_sym!(f, "UserService", "Logger", "User", "handler"); }
#[test] fn golden_ts_type_bindings() { let f = fixture!("../tests/fixtures/typescript/routes.ts", "ts"); assert_bind!(f, "users" => "User", "logger" => "Logger"); }
#[test] fn golden_python_symbols() { let f = fixture!("../tests/fixtures/python/models.py", "py"); assert_sym!(f, "User", "UserService", "UserRepository", "Logger"); }
#[test] fn golden_python_type_bindings() { let f = fixture!("../tests/fixtures/python/models.py", "py"); assert_bind!(f, "repository" => "UserRepository", "message" => "str"); }
#[test] fn golden_rust_symbols() { let f = fixture!("../tests/fixtures/rust/service.rs", "rs"); assert_sym!(f, "User", "UserService", "Repository", "Logger", "fmt"); }
#[test] fn golden_rust_type_bindings() { let f = fixture!("../tests/fixtures/rust/service.rs", "rs"); assert_bind!(f, "users" => "Vec", "logger" => "Logger"); }
#[test] fn golden_rust_heritage() { let f = fixture!("../tests/fixtures/rust/service.rs", "rs"); assert_herit!(f, "Repository"); }
#[test] fn golden_php_symbols() { let f = fixture!("../tests/fixtures/php/Controller.php", "php"); assert_sym!(f, "UserController", "AbstractController", "UserService", "User"); }
#[test] fn golden_php_heritage() { let f = fixture!("../tests/fixtures/php/Controller.php", "php"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_go_symbols() { let f = fixture!("../tests/fixtures/go/service.go", "go"); assert_sym!(f, "User", "UserService", "Repository", "Logger", "NewUserService"); }
#[test] fn golden_go_type_bindings() { let f = fixture!("../tests/fixtures/go/service.go", "go"); assert_bind!(f, "id" => "int", "repo" => "Repository", "logger" => "Logger"); }
#[test] fn golden_java_symbols() { let f = fixture!("../tests/fixtures/java/Service.java", "java"); assert_sym!(f, "User", "UserService", "Repository", "Logger", "AbstractController", "UserController"); }
#[test] fn golden_java_type_bindings() { let f = fixture!("../tests/fixtures/java/Service.java", "java"); assert_bind!(f, "id" => "int", "name" => "String", "repo" => "Repository", "logger" => "Logger"); }
#[test] fn golden_java_inheritance() { let f = fixture!("../tests/fixtures/java/Service.java", "java"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_c_symbols() { let f = fixture!("../tests/fixtures/c/types.c", "c"); assert_sym!(f, "User", "Repository", "Logger", "UserService"); }
#[test] fn golden_c_symbols_basic() { let f = fixture!("../tests/fixtures/c/types.c", "c"); assert!(f.symbols.len() >= 9); assert!(f.type_bindings.is_empty()); }
#[test] fn golden_cpp_symbols() { let f = fixture!("../tests/fixtures/cpp/service.cpp", "cpp"); assert_sym!(f, "User", "Repository", "Logger", "UserService", "AbstractController", "UserController"); }
#[test] fn golden_cpp_type_bindings() { let f = fixture!("../tests/fixtures/cpp/service.cpp", "cpp"); assert_bind!(f, "id" => "int", "name" => "string"); }
#[test] fn golden_cpp_inheritance() { let f = fixture!("../tests/fixtures/cpp/service.cpp", "cpp"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_csharp_symbols() { let f = fixture!("../tests/fixtures/csharp/Service.cs", "cs"); assert_sym!(f, "User", "UserService", "IRepository", "ILogger", "AbstractController", "UserController"); }
#[test] fn golden_csharp_type_bindings() { let f = fixture!("../tests/fixtures/csharp/Service.cs", "cs"); assert_bind!(f, "repo" => "IRepository", "logger" => "ILogger", "Id" => "int"); }
#[test] fn golden_csharp_inheritance() { let f = fixture!("../tests/fixtures/csharp/Service.cs", "cs"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_ruby_symbols() { let f = fixture!("../tests/fixtures/ruby/service.rb", "rb"); assert_sym!(f, "User", "Logger", "Repository", "UserService", "AbstractController", "UserController"); }
#[test] fn golden_ruby_inheritance() { let f = fixture!("../tests/fixtures/ruby/service.rb", "rb"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_kotlin_symbols() { let f = fixture!("../tests/fixtures/kotlin/Service.kt", "kt"); assert_sym!(f, "User", "UserService", "Repository", "Logger", "AbstractController", "UserController"); }
#[test] fn golden_kotlin_inheritance() { let f = fixture!("../tests/fixtures/kotlin/Service.kt", "kt"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_swift_symbols() { let f = fixture!("../tests/fixtures/swift/Service.swift", "swift"); assert_sym!(f, "User", "UserService", "Repository", "Logger", "AbstractController", "UserController"); }
#[test] fn golden_swift_type_bindings() { let f = fixture!("../tests/fixtures/swift/Service.swift", "swift"); assert_bind!(f, "id" => "Int", "repo" => "Repository"); }
#[test] fn golden_dart_symbols() { let f = fixture!("../tests/fixtures/dart/service.dart", "dart"); assert_sym!(f, "User", "UserService", "Repository", "Logger", "AbstractController", "UserController"); }
#[test] fn golden_dart_inheritance() { let f = fixture!("../tests/fixtures/dart/service.dart", "dart"); assert_herit!(f, "AbstractController"); }
#[test] fn golden_dart_methods() { let f = fixture!("../tests/fixtures/dart/service.dart", "dart"); assert_sym!(f, "findById", "save", "log", "index", "show", "render", "formatName"); }
#[test] fn golden_dart_calls() { let f = fixture!("../tests/fixtures/dart/service.dart", "dart"); let calls: Vec<&str> = f.calls.iter().map(|c| c.callee_name.as_str()).collect(); assert!(!calls.is_empty(), "Dart should extract calls"); assert!(calls.contains(&"print") || calls.contains(&"render") || calls.contains(&"findById"), "Dart calls should contain render/findIds/print, got: {:?}", calls); }
#[test] fn golden_bash_symbols() { let f = fixture!("../tests/fixtures/bash/deploy.sh", "sh"); assert_sym!(f, "log_message", "check_prerequisites", "deploy", "rollback", "main"); }

#[test] fn golden_type_bindings_extracted_all_languages() {
    for (p, e, name) in &[
        ("../tests/fixtures/typescript/routes.ts", "ts", "TS"), ("../tests/fixtures/python/models.py", "py", "Python"),
        ("../tests/fixtures/rust/service.rs", "rs", "Rust"), ("../tests/fixtures/go/service.go", "go", "Go"),
        ("../tests/fixtures/java/Service.java", "java", "Java"), ("../tests/fixtures/cpp/service.cpp", "cpp", "C++"),
        ("../tests/fixtures/csharp/Service.cs", "cs", "C#"), ("../tests/fixtures/kotlin/Service.kt", "kt", "Kotlin"),
        ("../tests/fixtures/swift/Service.swift", "swift", "Swift"), ("../tests/fixtures/dart/service.dart", "dart", "Dart"),
    ] {

        let f = parse(&fs::read_to_string(p).unwrap(), e);
        assert!(!f.type_bindings.is_empty(), "{}: no type bindings (got {} calls, {} symbols)", name, f.calls.len(), f.symbols.len());
    }
}

#[test] fn golden_all_languages_symbols() {
    for (p, e, name, syms) in &[
        ("../tests/fixtures/typescript/routes.ts", "ts", "TS", vec!["UserService", "Logger", "User"]),
        ("../tests/fixtures/python/models.py", "py", "Python", vec!["User", "UserService", "Logger"]),
        ("../tests/fixtures/rust/service.rs", "rs", "Rust", vec!["User", "UserService", "Repository", "Logger"]),
        ("../tests/fixtures/go/service.go", "go", "Go", vec!["User", "UserService", "Repository"]),
        ("../tests/fixtures/java/Service.java", "java", "Java", vec!["User", "UserService", "Repository", "Logger"]),
        ("../tests/fixtures/c/types.c", "c", "C", vec!["User", "Repository", "Logger", "UserService"]),
        ("../tests/fixtures/cpp/service.cpp", "cpp", "C++", vec!["User", "Repository", "Logger", "UserService"]),
        ("../tests/fixtures/csharp/Service.cs", "cs", "C#", vec!["User", "UserService", "IRepository", "ILogger"]),
        ("../tests/fixtures/ruby/service.rb", "rb", "Ruby", vec!["User", "UserService", "Logger", "Repository"]),
        ("../tests/fixtures/php/Controller.php", "php", "PHP", vec!["UserController", "UserService", "User"]),
        ("../tests/fixtures/kotlin/Service.kt", "kt", "Kotlin", vec!["User", "UserService", "Repository", "Logger"]),
        ("../tests/fixtures/swift/Service.swift", "swift", "Swift", vec!["User", "UserService", "Repository", "Logger"]),
        ("../tests/fixtures/dart/service.dart", "dart", "Dart", vec!["User", "UserService", "Repository", "Logger"]),
        ("../tests/fixtures/bash/deploy.sh", "sh", "Bash", vec!["deploy", "rollback", "main", "log_message"]),
    ] {
        let f = parse(&fs::read_to_string(p).unwrap(), e);
        assert!(!f.symbols.is_empty(), "{}: no symbols", name);
        let n: Vec<&str> = f.symbols.iter().map(|s| s.name.as_str()).collect();
        for s in syms { assert!(n.contains(s), "{}: '{}' not found in {:?}", name, s, n); }
    }
}
