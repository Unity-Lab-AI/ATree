//! Architecture boundary enforcement.
//!
//! Allows declaring module layers and permitted import directions.
//! Violations are detected during indexing and exposed via MCP tools and CLI.
//!
//! ## Configuration
//!
//! Boundaries are declared in `.atree/boundaries.json`:
//!
//! ```json
//! {
//!   "layers": [
//!     {"name": "presentation", "paths": ["src/ui/", "src/components/", "src/pages/"]},
//!     {"name": "domain", "paths": ["src/models/", "src/services/", "src/domain/"]},
//!     {"name": "data", "paths": ["src/repositories/", "src/db/", "src/api/"]}
//!   ],
//!   "rules": [
//!     {"name": "pres-to-domain", "from": "presentation", "to": "domain", "allowed": true},
//!     {"name": "domain-to-data", "from": "domain", "to": "data", "allowed": true},
//!     {"name": "pres-no-data", "from": "presentation", "to": "data", "allowed": false},
//!     {"name": "data-no-up", "from": "data", "to": "presentation", "allowed": false}
//!   ]
//! }
//! ```
//!
//! ## Detection
//!
//! During cross-file analysis, every cross-file CALLS or IMPORTS edge is checked
//! against the boundary rules. Violations are stored in the `boundary_violations`
//! table and exposed via `query boundary-check` and `mcp architecture_boundary_check`.

use rustc_hash::FxHashMap;
use serde::{Serialize, Deserialize};
use std::path::Path;

/// A named layer in the architecture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub name: String,
    pub paths: Vec<String>,
}

/// A rule governing allowed imports between layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryRule {
    pub name: String,
    pub from: String,
    pub to: String,
    pub allowed: bool,
}

/// Complete boundary configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BoundaryConfig {
    pub layers: Vec<Layer>,
    pub rules: Vec<BoundaryRule>,
}

impl BoundaryConfig {
    /// Load boundary config from a JSON file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read boundary config: {}", e))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse boundary config: {}", e))
    }

    /// Load from `.atree/boundaries.json` in the project root.
    pub fn load_default(root: &Path) -> Option<Self> {
        let path = root.join(".atree/boundaries.json");
        if path.exists() {
            Self::load(&path).ok()
        } else {
            None
        }
    }

    /// Determine which layer a file path belongs to.
    pub fn resolve_layer(&self, file_path: &str) -> Option<&str> {
        for layer in &self.layers {
            for prefix in &layer.paths {
                if file_path.starts_with(prefix) || file_path.contains(prefix) {
                    return Some(&layer.name);
                }
            }
        }
        None
    }

    /// Check if an import from `from_path` to `to_path` is allowed.
    /// Returns Ok(()) if allowed, Err(rule_name) if violated.
    pub fn check_import(&self, from_path: &str, to_path: &str) -> Result<(), String> {
        let from_layer = match self.resolve_layer(from_path) {
            Some(l) => l,
            None => return Ok(()), // files outside declared layers are not governed
        };
        let to_layer = match self.resolve_layer(to_path) {
            Some(l) => l,
            None => return Ok(()),
        };

        // Same layer is always allowed
        if from_layer == to_layer {
            return Ok(());
        }

        // Check rules
        for rule in &self.rules {
            if rule.from == from_layer && rule.to == to_layer {
                if rule.allowed {
                    return Ok(());
                } else {
                    return Err(rule.name.clone());
                }
            }
        }

        // No matching rule — default allow (conservative)
        Ok(())
    }
}

/// A detected boundary violation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryViolation {
    pub rule_name: String,
    pub from_file: String,
    pub to_file: String,
    pub from_layer: String,
    pub to_layer: String,
    pub violation_kind: String, // "import" or "call"
    pub line: usize,
    pub symbol_name: String,
}

/// Check all cross-file edges against boundary rules.
pub fn detect_violations(
    config: &BoundaryConfig,
    edges: &[(String, String, String, usize, String)], // (from_file, to_file, kind, line, symbol)
) -> Vec<BoundaryViolation> {
    let mut violations = Vec::new();

    for (from_file, to_file, kind, line, symbol) in edges {
        if let Err(rule_name) = config.check_import(from_file, to_file) {
            let from_layer = config.resolve_layer(from_file)
                .unwrap_or("?").to_string();
            let to_layer = config.resolve_layer(to_file)
                .unwrap_or("?").to_string();
            violations.push(BoundaryViolation {
                rule_name,
                from_file: from_file.clone(),
                to_file: to_file.clone(),
                from_layer,
                to_layer,
                violation_kind: kind.clone(),
                line: *line,
                symbol_name: symbol.clone(),
            });
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BoundaryConfig {
        BoundaryConfig {
            layers: vec![
                Layer { name: "presentation".to_string(), paths: vec!["src/ui/".to_string(), "src/pages/".to_string()] },
                Layer { name: "domain".to_string(), paths: vec!["src/services/".to_string(), "src/models/".to_string()] },
                Layer { name: "data".to_string(), paths: vec!["src/repositories/".to_string(), "src/db/".to_string()] },
            ],
            rules: vec![
                BoundaryRule { name: "pres-to-domain".to_string(), from: "presentation".to_string(), to: "domain".to_string(), allowed: true },
                BoundaryRule { name: "domain-to-data".to_string(), from: "domain".to_string(), to: "data".to_string(), allowed: true },
                BoundaryRule { name: "pres-no-data".to_string(), from: "presentation".to_string(), to: "data".to_string(), allowed: false },
                BoundaryRule { name: "data-no-up".to_string(), from: "data".to_string(), to: "presentation".to_string(), allowed: false },
            ],
        }
    }

    #[test]
    fn test_layer_resolution() {
        let config = test_config();
        assert_eq!(config.resolve_layer("src/ui/Button.tsx"), Some("presentation"));
        assert_eq!(config.resolve_layer("src/services/UserService.ts"), Some("domain"));
        assert_eq!(config.resolve_layer("src/repositories/UserRepo.ts"), Some("data"));
        assert_eq!(config.resolve_layer("src/utils/helpers.ts"), None);
    }

    #[test]
    fn test_allowed_imports() {
        let config = test_config();
        // presentation → domain: allowed
        assert!(config.check_import("src/ui/Button.tsx", "src/services/UserService.ts").is_ok());
        // domain → data: allowed
        assert!(config.check_import("src/services/UserService.ts", "src/repositories/UserRepo.ts").is_ok());
        // same layer: always allowed
        assert!(config.check_import("src/ui/Button.tsx", "src/pages/Home.tsx").is_ok());
    }

    #[test]
    fn test_violations() {
        let config = test_config();
        // presentation → data: NOT allowed
        let result = config.check_import("src/ui/Button.tsx", "src/repositories/UserRepo.ts");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "pres-no-data");
    }

    #[test]
    fn test_detect_violations() {
        let config = test_config();
        let edges = vec![
            ("src/ui/Button.tsx".to_string(), "src/services/UserService.ts".to_string(), "CALLS".to_string(), 10, "handleClick".to_string()),
            ("src/ui/Button.tsx".to_string(), "src/repositories/UserRepo.ts".to_string(), "IMPORTS".to_string(), 1, "UserRepo".to_string()),
            ("src/services/UserService.ts".to_string(), "src/repositories/UserRepo.ts".to_string(), "CALLS".to_string(), 20, "findById".to_string()),
        ];
        let violations = detect_violations(&config, &edges);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule_name, "pres-no-data");
        assert_eq!(violations[0].from_file, "src/ui/Button.tsx");
        assert_eq!(violations[0].to_file, "src/repositories/UserRepo.ts");
    }
}
