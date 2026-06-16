//! Import graph for A*-based import resolution.
//!
//! Builds a directed graph where nodes are file paths and edges represent
//! import relationships. Used by the A* resolver to find definition files
//! through transitive include chains (C/C++ `#include` closures, etc.).
//!
//! The A* heuristic uses depth difference from entry-point files (files with
//! no incoming imports). This is admissible on the import DAG because each
//! import edge changes depth by at most 1.

use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{BinaryHeap, VecDeque};
use std::cmp::Reverse;

/// A wrapper around f64 that implements Ord for use in BinaryHeap.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrdF64(f64);

impl Eq for OrdF64 {}

impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// An import edge from one file to another.
#[derive(Debug, Clone)]
pub struct ImportEdge {
    pub from_file: String,
    pub to_file: String,
    pub confidence: f64,
    pub import_source: String,
}

/// A directed graph of file-level import relationships.
///
/// `adjacency[file]` = list of files it imports (outgoing edges).
/// `reverse_adjacency[file]` = list of files that import it (incoming edges).
#[derive(Debug, Clone, Default)]
pub struct ImportGraph {
    adjacency: FxHashMap<String, Vec<(String, f64)>>,
    reverse_adjacency: FxHashMap<String, Vec<(String, f64)>>,
    all_files: FxHashSet<String>,
}

impl ImportGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the import graph from parsed files.
    /// For each file, resolve its imports to candidate target files using
    /// the per-language resolver, then add edges.
    pub fn from_parsed_files(
        parsed_files: &[crate::semantic::ParsedFile],
    ) -> Self {
        let mut graph = Self::new();

        for pf in parsed_files {
            graph.all_files.insert(pf.path.clone());
        }

        let all_paths: Vec<String> = graph.all_files.iter().cloned().collect();

        for pf in parsed_files {
            for imp in &pf.imports {
                if let Some((target_path, confidence)) = super::import_resolver::resolve_import(
                    &imp.source,
                    &pf.path,
                    &all_paths,
                    pf.language,
                ) {
                    graph.add_edge(&pf.path, &target_path, confidence, &imp.source);
                }
            }
        }

        graph
    }

    pub fn add_edge(&mut self, from: &str, to: &str, confidence: f64, _import_source: &str) {
        self.adjacency
            .entry(from.to_string())
            .or_default()
            .push((to.to_string(), confidence));
        self.reverse_adjacency
            .entry(to.to_string())
            .or_default()
            .push((from.to_string(), confidence));
        self.all_files.insert(from.to_string());
        self.all_files.insert(to.to_string());
    }

    pub fn has_file(&self, file: &str) -> bool {
        self.all_files.contains(file)
    }

    /// Get files directly imported by `file`.
    pub fn imports_of(&self, file: &str) -> &[(String, f64)] {
        self.adjacency.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get files that directly import `file`.
    pub fn imported_by(&self, file: &str) -> &[(String, f64)] {
        self.reverse_adjacency.get(file).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn file_count(&self) -> usize {
        self.all_files.len()
    }

    pub fn edge_count(&self) -> usize {
        self.adjacency.values().map(|v| v.len()).sum()
    }

    /// Compute BFS depths from a set of root files (entry points).
    /// Used as the admissible heuristic for A*.
    pub fn compute_depths(&self, roots: &[String]) -> FxHashMap<String, i32> {
        let mut depth = FxHashMap::default();
        let mut visited = FxHashSet::default();
        let mut q = VecDeque::new();

        for root in roots {
            depth.insert(root.clone(), 0);
            visited.insert(root.clone());
            q.push_back(root.clone());
        }

        while let Some(curr) = q.pop_front() {
            let curr_depth = depth[&curr];
            for (nei, _) in self.imports_of(&curr) {
                if !visited.contains(nei) {
                    visited.insert(nei.clone());
                    depth.insert(nei.clone(), curr_depth + 1);
                    q.push_back(nei.clone());
                }
            }
        }
        depth
    }

    /// A* search from `start_file` to find a file matching `goal_predicate`.
    ///
    /// The heuristic uses depth difference: `h(n) = min(|depth[n] - depth[g]| for g in goals)`.
    /// Edge weight is `1.0 / confidence`, so higher-confidence imports are cheaper.
    ///
    /// Returns `(path, nodes_expanded)` or `None` if no path exists.
    pub fn astar_find(
        &self,
        start_file: &str,
        goal_predicate: &dyn Fn(&str) -> bool,
        depths: &FxHashMap<String, i32>,
    ) -> Option<(Vec<String>, usize)> {
        if !self.has_file(start_file) {
            return None;
        }

        let goals: Vec<String> = self.all_files.iter()
            .filter(|f| goal_predicate(f))
            .cloned()
            .collect();

        if goals.is_empty() {
            return None;
        }

        let mut open_set: BinaryHeap<(Reverse<OrdF64>, String)> = BinaryHeap::new();
        let mut came_from: FxHashMap<String, String> = FxHashMap::default();
        let mut g_score: FxHashMap<String, f64> = FxHashMap::default();
        let mut closed: FxHashSet<String> = FxHashSet::default();

        g_score.insert(start_file.to_string(), 0.0);
        let h_start = Self::min_heuristic_to_goals(start_file, &goals, depths);
        open_set.push((Reverse(OrdF64(h_start)), start_file.to_string()));
        let mut expanded = 0usize;

        while let Some((_, current)) = open_set.pop() {
            if goal_predicate(&current) {
                let mut path = vec![current.clone()];
                let mut curr = current;
                while let Some(prev) = came_from.get(&curr) {
                    path.push(prev.clone());
                    curr = prev.clone();
                }
                path.reverse();
                return Some((path, expanded));
            }

            if closed.contains(&current) {
                continue;
            }
            closed.insert(current.clone());
            expanded += 1;

            for (nei, confidence) in self.imports_of(&current) {
                if closed.contains(nei) {
                    continue;
                }
                let edge_weight = 1.0 / confidence.max(0.1);
                let tentative_g = g_score.get(&current).unwrap_or(&f64::MAX) + edge_weight;
                if tentative_g < *g_score.get(nei).unwrap_or(&f64::MAX) {
                    came_from.insert(nei.clone(), current.clone());
                    g_score.insert(nei.clone(), tentative_g);
                    let h = Self::min_heuristic_to_goals(nei, &goals, depths);
                    let f = tentative_g + h;
                    open_set.push((Reverse(OrdF64(f)), nei.clone()));
                }
            }
        }
        None
    }

    /// Minimum depth-difference heuristic across all goal nodes.
    fn min_heuristic_to_goals(
        node: &str,
        goals: &[String],
        depths: &FxHashMap<String, i32>,
    ) -> f64 {
        let node_depth = depths.get(node).copied().unwrap_or(i32::MAX);
        goals.iter()
            .filter_map(|g| depths.get(g))
            .map(|gd| (node_depth - gd).abs() as f64)
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(f64::MAX)
    }

    /// BFS expansion count baseline for comparison with A*.
    pub fn bfs_expanded(
        &self,
        start: &str,
        goal_predicate: &dyn Fn(&str) -> bool,
    ) -> usize {
        if !self.has_file(start) {
            return 0;
        }
        let mut visited = FxHashSet::default();
        let mut q = VecDeque::new();
        visited.insert(start.to_string());
        q.push_back(start.to_string());
        let mut expanded = 0usize;
        while let Some(curr) = q.pop_front() {
            expanded += 1;
            if goal_predicate(&curr) {
                return expanded;
            }
            for (nei, _) in self.imports_of(&curr) {
                if !visited.contains(nei) {
                    visited.insert(nei.clone());
                    q.push_back(nei.clone());
                }
            }
        }
        expanded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_graph() -> ImportGraph {
        let mut g = ImportGraph::new();
        // a -> b -> c -> d
        // a -> c (direct)
        g.add_edge("a.rs", "b.rs", 0.95, "use b");
        g.add_edge("b.rs", "c.rs", 0.90, "use c");
        g.add_edge("c.rs", "d.rs", 0.85, "use d");
        g.add_edge("a.rs", "c.rs", 0.80, "use c");
        g
    }

    #[test]
    fn test_construction() {
        let g = make_graph();
        assert_eq!(g.file_count(), 4);
        assert_eq!(g.edge_count(), 4);
        assert!(g.has_file("a.rs"));
        assert!(!g.has_file("nonexistent.rs"));
    }

    #[test]
    fn test_imports_of() {
        let g = make_graph();
        let imports: Vec<&str> = g.imports_of("a.rs").iter().map(|(f, _)| f.as_str()).collect();
        assert!(imports.contains(&"b.rs"));
        assert!(imports.contains(&"c.rs"));
    }

    #[test]
    fn test_imported_by() {
        let g = make_graph();
        let importers: Vec<&str> = g.imported_by("c.rs").iter().map(|(f, _)| f.as_str()).collect();
        assert!(importers.contains(&"b.rs"));
        assert!(importers.contains(&"a.rs"));
    }

    #[test]
    fn test_compute_depths() {
        let g = make_graph();
        let roots = vec!["a.rs".to_string()];
        let depths = g.compute_depths(&roots);
        assert_eq!(depths.get("a.rs"), Some(&0));
        assert_eq!(depths.get("b.rs"), Some(&1));
        // c.rs is reachable at depth 1 (a->c direct) or depth 2 (a->b->c); BFS gives 1
        assert_eq!(depths.get("c.rs"), Some(&1));
        assert_eq!(depths.get("d.rs"), Some(&2));
    }

    #[test]
    fn test_astar_direct() {
        let g = make_graph();
        let roots = vec!["a.rs".to_string()];
        let depths = g.compute_depths(&roots);
        let result = g.astar_find("a.rs", &|f| f == "b.rs", &depths);
        assert!(result.is_some());
        let (path, expanded) = result.unwrap();
        assert_eq!(path, vec!["a.rs", "b.rs"]);
        assert!(expanded <= 2);
    }

    #[test]
    fn test_astar_transitive() {
        let g = make_graph();
        let roots = vec!["a.rs".to_string()];
        let depths = g.compute_depths(&roots);
        let result = g.astar_find("a.rs", &|f| f == "d.rs", &depths);
        assert!(result.is_some());
        let (path, _) = result.unwrap();
        assert_eq!(path.first(), Some(&"a.rs".to_string()));
        assert_eq!(path.last(), Some(&"d.rs".to_string()));
    }

    #[test]
    fn test_astar_no_path() {
        let g = make_graph();
        let roots = vec!["a.rs".to_string()];
        let depths = g.compute_depths(&roots);
        // d.rs doesn't import anything, so searching from d.rs to a.rs finds nothing
        let result = g.astar_find("d.rs", &|f| f == "nonexistent.rs", &depths);
        assert!(result.is_none());
    }

    #[test]
    fn test_astar_vs_bfs() {
        let g = make_graph();
        let roots = vec!["a.rs".to_string()];
        let depths = g.compute_depths(&roots);
        let (_path, astar_expanded) = g.astar_find("a.rs", &|f| f == "d.rs", &depths).unwrap();
        let bfs_expanded = g.bfs_expanded("a.rs", &|f| f == "d.rs");
        // A* should expand no more nodes than BFS
        assert!(astar_expanded <= bfs_expanded);
    }

    #[test]
    fn test_multiple_goals() {
        let mut g = ImportGraph::new();
        g.add_edge("main.rs", "auth.rs", 0.95, "mod auth");
        g.add_edge("main.rs", "db.rs", 0.95, "mod db");
        g.add_edge("auth.rs", "login.rs", 0.90, "mod login");
        g.add_edge("db.rs", "login.rs", 0.90, "mod login");

        let roots = vec!["main.rs".to_string()];
        let depths = g.compute_depths(&roots);

        // Search for any file containing "login" — should find login.rs
        let result = g.astar_find("main.rs", &|f| f.contains("login"), &depths);
        assert!(result.is_some());
        let (path, _) = result.unwrap();
        assert_eq!(path.last(), Some(&"login.rs".to_string()));
    }
}
