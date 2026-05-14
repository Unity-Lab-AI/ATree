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
    if cleaned.starts_with("crate::") {
        let path = &cleaned[7..]; // strip "crate::"
        let file_path = rust_path_to_file(path);
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.95));
        }
        // Try as module/mod.rs
        let mod_rs = format!("{}/mod.rs", path.replace("::", "/"));
        if let Some(m) = find_file_in_list(&mod_rs, all_files) {
            return Some((m, 0.95));
        }
    }

    // `use super::...` — resolve relative to parent module
    if cleaned.starts_with("super::") {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let parent_dir = Path::new(&from_dir).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let path = &cleaned[7..]; // strip "super::"
        let file_path = format!("{}/{}", parent_dir, rust_path_to_file(path));
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.95));
        }
    }

    // `use self::...` — resolve in same module
    if cleaned.starts_with("self::") {
        let from_dir = Path::new(from_file).parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let path = &cleaned[6..]; // strip "self::"
        let file_path = format!("{}/{}", from_dir, rust_path_to_file(path));
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.95));
        }
    }

    // Bare `use X::Y` — could be crate root or external
    if !cleaned.starts_with("crate::") && !cleaned.starts_with("super::") && !cleaned.starts_with("self::") {
        let file_path = rust_path_to_file(cleaned);
        if let Some(m) = find_file_in_list(&file_path, all_files) {
            return Some((m, 0.85));
        }
    }

    None
}

fn rust_path_to_file(path: &str) -> String {
    let parts: Vec<&str> = path.split("::").collect();
    if parts.len() == 1 {
        format!("{}.rs", parts[0])
    } else {
        let module_path = parts[..parts.len() - 1].join("/");
        format!("{}/{}.rs", module_path, parts.last().unwrap())
    }
}

/// TypeScript/JavaScript import resolution.
/// Handles: `import X from 'Y'`, `import { X } from 'Y'`, `require('Y')`
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

    // Bare module specifier — could be node_modules or path alias
    // Try as a relative path from common roots
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


/// Find a file in the list that matches the given path.
/// Uses suffix matching - the import path should end with the file path.
fn find_file_in_list(target: &str, all_files: &[String]) -> Option<String> {
    let target = target.replace('\\', "/");
    for file in all_files {
        let norm = file.replace('\\', "/");
        if norm == target || norm.ends_with(&target) {
            return Some(file.clone());
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
