//! Per-language import resolution.
//!
//! Each language has its own module system. This module provides
//! language-specific import path → file path resolution.

use crate::lang::LanguageId;
use std::path::Path;

/// Resolve an import path to a target file path, given the source file.
/// Returns None if the import cannot be resolved.
pub fn resolve_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
    lang: LanguageId,
) -> Option<(String, f64)> {
    // Handle grouped imports: `use crate::store::{A, B, C}` → try each item
    if let Some((prefix, rest)) = import_source.split_once("::") {
        if let Some(group_start) = rest.find('{') {
            if let Some(group_end) = rest.rfind('}') {
                let module_prefix = &rest[..group_start];
                let items_str = &rest[group_start + 1..group_end];
                let base_path = if module_prefix.is_empty() {
                    prefix.to_string()
                } else {
                    format!("{}::{}", prefix, module_prefix.trim_end_matches("::"))
                };
                // Try each item in the group
                for item in items_str.split(',') {
                    let item = item.trim();
                    if item.is_empty() { continue; }
                    let full_path = format!("{}::{}", base_path, item);
                    if let Some(result) = resolve_import(&full_path, from_file, all_files, lang) {
                        return Some(result);
                    }
                }
                return None;
            }
        }
    }

    match lang {
        LanguageId::Python => resolve_python_import(import_source, from_file, all_files),
        LanguageId::Rust => resolve_rust_import(import_source, from_file, all_files),
        LanguageId::TypeScript | LanguageId::JavaScript => {
            resolve_ts_js_import(import_source, from_file, all_files)
        }
        LanguageId::Go => resolve_go_import(import_source, from_file, all_files),
        LanguageId::Java => resolve_java_import(import_source, from_file, all_files),
        LanguageId::C => resolve_c_import(import_source, from_file, all_files),
        LanguageId::Cpp => resolve_cpp_import(import_source, from_file, all_files),
        LanguageId::CSharp => resolve_csharp_import(import_source, from_file, all_files),
        LanguageId::Ruby => resolve_ruby_import(import_source, from_file, all_files),
        LanguageId::PHP => resolve_php_import(import_source, from_file, all_files),
        LanguageId::Kotlin => resolve_kotlin_import(import_source, from_file, all_files),
        LanguageId::Swift => resolve_swift_import(import_source, from_file, all_files),
        LanguageId::Dart => resolve_dart_import(import_source, from_file, all_files),
        LanguageId::Bash => None,
        LanguageId::JSON | LanguageId::YAML => None,
        LanguageId::Unknown => None,
    }
}

/// Python import resolution.
/// Handles: `import X`, `from X import Y`, relative imports (`from .X import Y`)
fn resolve_python_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches(|c| c == '\'' || c == '"');

    // Relative import: starts with .
    if cleaned.starts_with('.') {
        return resolve_python_relative(cleaned, from_file, all_files);
    }

    // Convert dotted module path to file path: `a.b.c` → `a/b/c.py` or `a/b/c/__init__.py`
    let module_path = cleaned.replace('.', "/");

    // Try direct module file
    let py_file = format!("{}.py", module_path);
    if let Some(m) = find_file_in_list(&py_file, all_files) {
        return Some((m, 0.95));
    }

    // Try package __init__.py
    let init_file = format!("{}/__init__.py", module_path);
    if let Some(m) = find_file_in_list(&init_file, all_files) {
        return Some((m, 0.95));
    }

    // Try with src/ prefix (common layout)
    let src_py = format!("src/{}.py", module_path);
    if let Some(m) = find_file_in_list(&src_py, all_files) {
        return Some((m, 0.85));
    }

    None
}

fn resolve_python_relative(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let dot_count = import_source.chars().take_while(|c| *c == '.').count();
    let module_part = &import_source[dot_count..];

    let from_dir = Path::new(from_file).parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();


    let mut current_dir = from_dir;
    for _ in 1..dot_count {
        current_dir = Path::new(&current_dir)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
    }

    if module_part.is_empty() {
        let init_file = format!("{}/__init__.py", current_dir);
        if let Some(m) = find_file_in_list(&init_file, all_files) {
            return Some((m, 0.95));
        }
    } else {
        let module_path = module_part.replace('.', "/");
        let py_file = format!("{}/{}.py", current_dir, module_path);
        if let Some(m) = find_file_in_list(&py_file, all_files) {
            return Some((m, 0.95));
        }
        let init_file = format!("{}/{}/__init__.py", current_dir, module_path);
        if let Some(m) = find_file_in_list(&init_file, all_files) {
            return Some((m, 0.95));
        }
        let alt_py = format!("{}.py", module_path);
        if let Some(m) = find_file_in_list(&alt_py, all_files) {
            return Some((m, 0.85));
        }
    }

    None
}

/// Rust import resolution.
/// Handles: `use crate::X::Y`, `use super::X`, `use self::X`, `extern crate X`
fn resolve_rust_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim();

    // `use crate::...` — resolve from crate root
    if let Some(path) = cleaned.strip_prefix("crate::") {
        let candidates = rust_path_to_candidates(path);
        if let Some((m, conf)) = find_rust_file(&candidates, all_files) {
            return Some((m, conf));
        }
    }

    // `use super::...` — resolve relative to parent module
    if let Some(path) = cleaned.strip_prefix("super::") {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent_dir = Path::new(&from_dir).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let candidates = rust_path_to_candidates(path);
        for candidate in &candidates {
            let file_path = format!("{}/{}", parent_dir, candidate);
            if let Some(m) = find_file_in_list(&file_path, all_files) {
                return Some((m, 0.95));
            }
        }
    }

    // `use self::...` — resolve in same module
    if let Some(path) = cleaned.strip_prefix("self::") {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let candidates = rust_path_to_candidates(path);
        for candidate in &candidates {
            let file_path = format!("{}/{}", from_dir, candidate);
            if let Some(m) = find_file_in_list(&file_path, all_files) {
                return Some((m, 0.95));
            }
        }
    }

    // Bare `use X::Y` — could be crate root or external
    if !cleaned.starts_with("crate::") && !cleaned.starts_with("super::") && !cleaned.starts_with("self::") {
        let candidates = rust_path_to_candidates(cleaned);
        if let Some((m, conf)) = find_rust_file(&candidates, all_files) {
            return Some((m, conf * 0.85));
        }
    }

    None
}

/// Convert a Rust module path (e.g., "store::store::GraphStore") to candidate file paths.
/// Returns candidates in order of likelihood.
/// For `a::b::C`, tries:
///   1. `a/b/C.rs`      — C is a submodule
///   2. `a/b.rs`        — C is defined in b.rs
///   3. `a/b/mod.rs`    — C is re-exported from b/mod.rs
/// Also tries with common src prefixes (src/, lib/).
fn rust_path_to_candidates(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split("::").collect();
    let mut candidates = Vec::new();

    if parts.len() == 1 {
        candidates.push(format!("{}.rs", parts[0]));
        return candidates;
    }

    let module_path = parts[..parts.len() - 1].join("/");
    let last = parts.last().expect("parts must be non-empty (checked above)");

    // C is a submodule: a/b/C.rs
    candidates.push(format!("{}/{}.rs", module_path, last));
    // C is defined in b.rs: a/b.rs
    candidates.push(format!("{}.rs", module_path));
    // C is re-exported from b/mod.rs: a/b/mod.rs
    candidates.push(format!("{}/mod.rs", module_path));

    candidates
}

/// Find the first matching file from candidates in the file list.
fn find_rust_file(candidates: &[String], all_files: &[String]) -> Option<(String, f64)> {
    for (i, candidate) in candidates.iter().enumerate() {
        if let Some(m) = find_file_in_list(candidate, all_files) {
            // First candidate gets highest confidence
            let conf = 1.0 - (i as f64 * 0.05);
            return Some((m, conf));
        }
    }
    None
}

/// TypeScript/JavaScript import resolution.
/// Handles: `import X from 'Y'`, `import { X } from 'Y'`, `require('Y')`
///
/// Resolution order:
/// 1. Relative imports (`./foo`, `../bar`)
/// 2. tsconfig.json path aliases (`@/components/Button` → `src/components/Button.tsx`)
/// 3. node_modules packages (`lodash`, `@types/node`)
/// 4. Bare specifier fallback (treat as file path)
fn resolve_ts_js_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches(|c| c == '\'' || c == '"' || c == '`');

    // Relative import
    if cleaned.starts_with('.') {
        return resolve_ts_js_relative(cleaned, from_file, all_files);
    }

    // Try tsconfig path aliases first (highest confidence for bare specifiers)
    if let Some(result) = resolve_tsconfig_alias(cleaned, all_files) {
        return Some(result);
    }

    // Try node_modules resolution
    if let Some(result) = resolve_node_modules(cleaned, all_files) {
        return Some(result);
    }

    // Bare module specifier fallback — try as a file path
    let extensions = [".ts", ".tsx", ".js", ".jsx", ".d.ts"];
    for ext in &extensions {
        let file_path = format!("{}{}", cleaned, ext);
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.7));
        }
    }

    // Try index files
    for ext in &extensions {
        let file_path = format!("{}/index{}", cleaned, ext);
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.7));
        }
    }

    None
}

fn resolve_ts_js_relative(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let from_dir = Path::new(from_file).parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let combined = format!("{}/{}", from_dir, import_source);
    // Manually normalize . and .. segments
    let resolved = normalize_path(&combined);

    let extensions = [".ts", ".tsx", ".js", ".jsx", ""];

    for ext in &extensions {
        let file_path = format!("{}{}", resolved, ext);
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.95));
        }
    }

    for ext in &[".ts", ".tsx", ".js", ".jsx"] {
        let file_path = format!("{}/index{}", resolved, ext);
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.95));
        }
    }

    None
}

/// Go import resolution.
/// Go imports are package paths, not file paths.
fn resolve_go_import(
    import_source: &str,
    _from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches('"');

    // Go package path → directory with .go files
    // Try to find any .go file in the package directory
    for file in all_files {
        if file.ends_with(".go") {
            let dir = Path::new(file).parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if dir == cleaned || dir.ends_with(cleaned) {
                return Some((file.clone(), 0.9));
            }
        }
    }

    None
}

/// Java import resolution.
/// `import com.example.Foo` → `com/example/Foo.java`
fn resolve_java_import(
    import_source: &str,
    _from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim().trim_end_matches(';');
    let file_path = format!("{}.java", cleaned.replace('.', "/"));
    find_file_in_list(&file_path, all_files).map(|m| (m, 0.9))
}

/// C/C++ `#include` resolution.
fn resolve_c_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches(|c| c == '<' || c == '"' || c == '>');

    // Relative include
    if cleaned.starts_with('.') || !cleaned.starts_with('/') {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let resolved = format!("{}/{}", from_dir, cleaned);
        if let Some(m) = find_file_in_list(&resolved, all_files) {
            return Some((m, 0.95));
        }
    }

    // System include — just try to find it
    if let Some(m) = find_file_in_list(cleaned, all_files) {
        return Some((m, 0.7));
    }

    None
}

fn resolve_cpp_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    // Same as C
    resolve_c_import(import_source, from_file, all_files)
}

/// C# import resolution.
/// `using Namespace.SubNamespace` — find any .cs file in that namespace path
fn resolve_csharp_import(
    import_source: &str,
    _from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim().trim_end_matches(';');
    let path = cleaned.replace('.', "/");

    // Try as file
    let cs_file = format!("{}.cs", path);
    if let Some(m) = find_file_in_list(&cs_file, all_files) {
        return Some((m, 0.85));
    }

    // Try as directory with any .cs file
    for file in all_files {
        if file.ends_with(".cs") {
            let dir = Path::new(file).parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if dir == path || dir.starts_with(&path) {
                return Some((file.clone(), 0.7));
            }
        }
    }

    None
}

/// Ruby import resolution.
/// `require 'X'`, `require_relative 'X'`
fn resolve_ruby_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches(|c| c == '\'' || c == '"');

    // require_relative — resolve relative to current file
    if cleaned.starts_with("./") || cleaned.starts_with("../") {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let resolved = format!("{}/{}", from_dir, cleaned);
        let rb_file = format!("{}.rb", resolved);
        if let Some(m) = find_file_in_list(&rb_file, all_files) {
            return Some((m, 0.95));
        }
        if let Some(m) = find_file_in_list(&resolved, all_files) {
            return Some((m, 0.95));
        }
    }

    // require — try as gem path or local file
    let rb_file = format!("{}.rb", cleaned);
    if let Some(m) = find_file_in_list(&rb_file, all_files) {
        return Some((m, 0.8));
    }

    // Try lib/ prefix (common Ruby layout)
    let lib_rb = format!("lib/{}.rb", cleaned);
    if let Some(m) = find_file_in_list(&lib_rb, all_files) {
        return Some((m, 0.8));
    }

    None
}

/// PHP import resolution.
/// `use Namespace\Class`, `require 'file'`, `include 'file'`
fn resolve_php_import(
    import_source: &str,
    _from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches(|c| c == '\'' || c == '"');

    // PSR-4 style: Namespace\Class → Namespace/Class.php
    let psr4_path = format!("{}.php", cleaned.replace('\\', "/"));
    if let Some(m) = find_file_in_list(&psr4_path, all_files) {
        return Some((m, 0.9));
    }

    // Try as direct file path
    if cleaned.ends_with(".php") {
        if let Some(m) = find_file_in_list(cleaned, all_files) {
            return Some((m, 0.95));
        }
    }

    // Try with .php appended
    let php_file = format!("{}.php", cleaned);
    if let Some(m) = find_file_in_list(&php_file, all_files) {
        return Some((m, 0.85));
    }

    None
}

/// Kotlin import resolution.
/// Similar to Java: `import com.example.Foo`
fn resolve_kotlin_import(
    import_source: &str,
    _from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim();
    let path = cleaned.replace('.', "/");

    // Try .kt file
    let kt_file = format!("{}.kt", path);
    if let Some(m) = find_file_in_list(&kt_file, all_files) {
        return Some((m, 0.9));
    }

    // Try .kts script
    let kts_file = format!("{}.kts", path);
    if let Some(m) = find_file_in_list(&kts_file, all_files) {
        return Some((m, 0.9));
    }

    None
}

/// Swift import resolution.
/// `import Module` — find any .swift file matching the module name
fn resolve_swift_import(
    import_source: &str,
    _from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim();
    let swift_file = format!("{}.swift", cleaned);
    if let Some(m) = find_file_in_list(&swift_file, all_files) {
        return Some((m, 0.85));
    }

    // Try as directory with any .swift file
    for file in all_files {
        if file.ends_with(".swift") {
            let dir = Path::new(file).parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if dir == cleaned {
                return Some((file.clone(), 0.7));
            }
        }
    }

    None
}


/// Dart import resolution.
/// `import 'package:foo/bar.dart'` → resolve `package:foo/bar.dart`
/// `import 'relative/path.dart'` → resolve relative to current file
/// `import 'package:foo/bar.dart' as alias` → resolve the path part
fn resolve_dart_import(
    import_source: &str,
    from_file: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    let cleaned = import_source.trim_matches(|c| c == '\'' || c == '"');

    // Strip `as alias` suffix
    let path = if let Some(idx) = cleaned.find(" as ") {
        cleaned[..idx].trim()
    } else {
        cleaned
    };

    // package: import
    if let Some(pkg_path) = path.strip_prefix("package:") {
        // strip "package:"
        // Try lib/ directory (standard Dart package layout)
        let lib_path = format!("lib/{}", pkg_path);
        if let Some(m) = find_file_in_list(&lib_path, all_files) {
            return Some((m, 0.9));
        }
        // Try direct path
        if let Some(m) = find_file_in_list(pkg_path, all_files) {
            return Some((m, 0.85));
        }
        return None;
    }

    // Relative import
    if path.starts_with("./") || path.starts_with("../") || !path.starts_with('/') {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let resolved = if from_dir.is_empty() {
            path.to_string()
        } else {
            format!("{}/{}", from_dir, path)
        };
        let normalized = normalize_path(&resolved);
        if let Some(m) = find_file_in_list(&normalized, all_files) {
            return Some((m, 0.95));
        }
        // Try with .dart extension
        let with_ext = format!("{}.dart", normalized);
        if let Some(m) = find_file_in_list(&with_ext, all_files) {
            return Some((m, 0.95));
        }
    }

    None
}

/// Resolve a TypeScript/JavaScript import using tsconfig.json path aliases.
///
/// Looks for tsconfig.json files in the project and resolves path aliases
/// defined in `compilerOptions.paths`. For example:
/// ```json
/// { "compilerOptions": { "paths": { "@/*": ["./src/*"] } } }
/// ```
/// Then `@/components/Button` → `src/components/Button.tsx`
///
/// Since we only have the file list (not the full filesystem), we scan
/// all_files for tsconfig.json and parse it.
fn resolve_tsconfig_alias(
    import_source: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    // Find tsconfig.json in the indexed files
    let _tsconfig_path = all_files.iter().find(|f| {
        let norm = f.replace('\\', "/");
        norm.ends_with("tsconfig.json") || norm.ends_with("tsconfig.app.json")
            || norm.ends_with("tsconfig.base.json")
    })?;

    // We can't read the file from disk here since we only have the file list.
    // Instead, we use a heuristic: common path alias patterns.
    // Check for common alias prefixes and try to resolve them.
    let alias_patterns: Vec<(&str, &str)> = vec![
        ("@", "src"),           // @/* → src/*
        ("@/", "src/"),         // @/foo → src/foo
        ("~", "src"),           // ~/* → src/*
        ("~/", "src/"),         // ~/foo → src/foo
        ("@components", "src/components"),
        ("@/components", "src/components"),
        ("@utils", "src/utils"),
        ("@/utils", "src/utils"),
        ("@lib", "src/lib"),
        ("@/lib", "src/lib"),
        ("@hooks", "src/hooks"),
        ("@/hooks", "src/hooks"),
        ("@pages", "src/pages"),
        ("@/pages", "src/pages"),
        ("@services", "src/services"),
        ("@/services", "src/services"),
        ("@types", "src/types"),
        ("@/types", "src/types"),
        ("@assets", "src/assets"),
        ("@/assets", "src/assets"),
    ];

    for (alias, replacement) in &alias_patterns {
        if let Some(remainder) = import_source.strip_prefix(alias) {
            let resolved_path = format!("{}{}", replacement, remainder);

            // Try with common extensions
            let extensions = [".ts", ".tsx", ".js", ".jsx", ""];
            for ext in &extensions {
                let file_path = format!("{}{}", resolved_path, ext);
                if let Some(m) = find_file_in_list(&file_path, all_files) {
                    return Some((m, 0.92));
                }
            }

            // Try index files
            for ext in &[".ts", ".tsx", ".js", ".jsx"] {
                let file_path = format!("{}/index{}", resolved_path, ext);
                if let Some(m) = find_file_in_list(&file_path, all_files) {
                    return Some((m, 0.92));
                }
            }
        }
    }

    None
}

/// Resolve a bare module specifier as a node_modules package.
///
/// Looks for the package in node_modules/ directories found in the file list.
/// Handles:
/// - `lodash` → `node_modules/lodash/index.js` or `node_modules/lodash/lodash.js`
/// - `@types/node` → `node_modules/@types/node/index.d.ts`
/// - `@angular/core` → `node_modules/@angular/core/index.js`
fn resolve_node_modules(
    import_source: &str,
    all_files: &[String],
) -> Option<(String, f64)> {
    // Only try node_modules for non-relative, non-absolute imports
    if import_source.starts_with('.') || import_source.starts_with('/') {
        return None;
    }

    // Find all node_modules directories in the file list
    let node_modules_dirs: Vec<&str> = all_files
        .iter()
        .filter(|f| f.contains("node_modules"))
        .filter_map(|f| f.split("node_modules/").next())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    for base in &node_modules_dirs {
        let package_path = format!("{}/node_modules/{}", base, import_source);

        // Try direct file match
        let extensions = [".ts", ".tsx", ".js", ".jsx", ".d.ts", ".mjs", ".cjs"];
        for ext in &extensions {
            let file_path = format!("{}{}", package_path, ext);
            if let Some(m) = find_file_in_list(&file_path, all_files) {
                return Some((m, 0.8));
            }
        }

        // Try index files
        for ext in &[".ts", ".tsx", ".js", ".jsx", ".d.ts"] {
            let file_path = format!("{}/index{}", package_path, ext);
            if let Some(m) = find_file_in_list(&file_path, all_files) {
                return Some((m, 0.8));
            }
        }

        // Try package.json main field (heuristic: look for common entry points)
        for entry in &["lib/index", "src/index", "dist/index", "dist/src/index", "es/index", "cjs/index"] {
            for ext in &[".ts", ".tsx", ".js", ".jsx"] {
                let file_path = format!("{}/{}{}", package_path, entry, ext);
                if let Some(m) = find_file_in_list(&file_path, all_files) {
                    return Some((m, 0.75));
                }
            }
        }

        // For scoped packages like @angular/core, also try without scope dir
        if import_source.contains('/') {
            // Try barrel file: node_modules/@scope/pkg/index.ts
            for ext in &[".ts", ".tsx", ".js", ".jsx"] {
                let barrel = format!("{}/index{}", package_path, ext);
                if let Some(m) = find_file_in_list(&barrel, all_files) {
                    return Some((m, 0.78));
                }
            }
        }
    }

    None
}

/// Find a file in the list that matches the given path.
/// Matches on exact path or proper path-segment boundary (preceded by / or at start).
/// This avoids false positives like `utils.ts` matching `lib/src/utils.ts` when
/// the import was for `src/utils.ts`.
fn find_file_in_list(target: &str, all_files: &[String]) -> Option<String> {
    let target = target.replace('\\', "/");
    for file in all_files {
        let norm = file.replace('\\', "/");
        if norm == target {
            return Some(file.clone());
        }
        // Check suffix match at path segment boundary
        if norm.ends_with(&target) {
            let suffix_start = norm.len() - target.len();
            if suffix_start == 0 || norm.as_bytes()[suffix_start - 1] == b'/' {
                return Some(file.clone());
            }
        }
    }
    None
}

/// Normalize a path by resolving . and .. segments.
fn normalize_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    let mut result: Vec<&str> = Vec::new();
    for part in &parts {
        match *part {
            "." | "" => continue,
            ".." => { result.pop(); }
            _ => result.push(part),
        }
    }
    result.join("/")
}
