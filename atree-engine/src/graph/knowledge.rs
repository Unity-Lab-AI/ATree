//! In-memory KnowledgeGraph with indexed adjacency.
//!
//! Modeled after GitNexusRelay's `createKnowledgeGraph()` from
//! `gitnexus/src/core/graph/graph.ts`. Provides O(1) node/edge lookup,
//! reverse-adjacency indexes, per-type relationship iteration, and
//! file-based node grouping for efficient incremental invalidation.
//!
//! This is the in-memory counterpart to the SQLite-backed `GraphStore`.
//! Use this for pipeline phases that need fast graph traversal; flush
//! to `GraphStore` for persistence.

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Serialize, Deserialize};

// ── Node / Edge types ──────────────────────────────────────────────────────

/// Stable node identifier.
pub type NodeId = String;

/// Stable edge identifier.
pub type EdgeId = String;

/// Relationship types in the code graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationshipType {
    Contains,
    Calls,
    Inherits,
    MethodOverrides,
    MethodImplements,
    Imports,
    Uses,
    Defines,
    Decorates,
    Implements,
    Extends,
    HasMethod,
    HasProperty,
    Accesses,
    MemberOf,
    StepInProcess,
    HandlesRoute,
    Fetches,
    HandlesTool,
    EntryPointOf,
    Wraps,
    Queries,
}

impl RelationshipType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contains => "CONTAINS",
            Self::Calls => "CALLS",
            Self::Inherits => "INHERITS",
            Self::MethodOverrides => "METHOD_OVERRIDES",
            Self::MethodImplements => "METHOD_IMPLEMENTS",
            Self::Imports => "IMPORTS",
            Self::Uses => "USES",
            Self::Defines => "DEFINES",
            Self::Decorates => "DECORATES",
            Self::Implements => "IMPLEMENTS",
            Self::Extends => "EXTENDS",
            Self::HasMethod => "HAS_METHOD",
            Self::HasProperty => "HAS_PROPERTY",
            Self::Accesses => "ACCESSES",
            Self::MemberOf => "MEMBER_OF",
            Self::StepInProcess => "STEP_IN_PROCESS",
            Self::HandlesRoute => "HANDLES_ROUTE",
            Self::Fetches => "FETCHES",
            Self::HandlesTool => "HANDLES_TOOL",
            Self::EntryPointOf => "ENTRY_POINT_OF",
            Self::Wraps => "WRAPS",
            Self::Queries => "QUERIES",
        }
    }
}

impl std::str::FromStr for RelationshipType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "CONTAINS" => Ok(Self::Contains),
            "CALLS" => Ok(Self::Calls),
            "INHERITS" => Ok(Self::Inherits),
            "METHOD_OVERRIDES" => Ok(Self::MethodOverrides),
            "METHOD_IMPLEMENTS" => Ok(Self::MethodImplements),
            "IMPORTS" => Ok(Self::Imports),
            "USES" => Ok(Self::Uses),
            "DEFINES" => Ok(Self::Defines),
            "DECORATES" => Ok(Self::Decorates),
            "IMPLEMENTS" => Ok(Self::Implements),
            "EXTENDS" => Ok(Self::Extends),
            "HAS_METHOD" => Ok(Self::HasMethod),
            "HAS_PROPERTY" => Ok(Self::HasProperty),
            "ACCESSES" => Ok(Self::Accesses),
            "MEMBER_OF" => Ok(Self::MemberOf),
            "STEP_IN_PROCESS" => Ok(Self::StepInProcess),
            "HANDLES_ROUTE" => Ok(Self::HandlesRoute),
            "FETCHES" => Ok(Self::Fetches),
            "HANDLES_TOOL" => Ok(Self::HandlesTool),
            "ENTRY_POINT_OF" => Ok(Self::EntryPointOf),
            "WRAPS" => Ok(Self::Wraps),
            "QUERIES" => Ok(Self::Queries),
            other => Err(format!("Unknown relationship type: {}", other)),
        }
    }
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: NodeId,
    pub label: String,
    pub properties: FxHashMap<String, String>,
}

/// An edge (relationship) in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: EdgeId,
    pub source_id: NodeId,
    pub target_id: NodeId,
    pub rel_type: RelationshipType,
    pub confidence: f64,
    pub reason: String,
    pub step: Option<usize>,
}

/// Per-edge evidence trace for audit/debug.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeEvidence {
    pub kind: String,
    pub weight: f64,
    pub note: Option<String>,
}

// ── KnowledgeGraph ─────────────────────────────────────────────────────────

/// In-memory knowledge graph with multi-index for fast traversal.
///
/// Indexes maintained:
/// - `node_map: NodeId → GraphNode` — O(1) node lookup
/// - `edge_map: EdgeId → GraphEdge` — O(1) edge lookup
/// - `edges_by_type: RelationshipType → FxHashMap<EdgeId, GraphEdge>` — O(1) per-type iteration
/// - `edge_ids_by_node: NodeId → FxHashSet<EdgeId>` — reverse adjacency for O(edges-touching-node) removal
/// - `node_ids_by_file: filePath → FxHashSet<NodeId>` — file-based grouping for incremental invalidation
pub struct KnowledgeGraph {
    node_map: FxHashMap<NodeId, GraphNode>,
    edge_map: FxHashMap<EdgeId, GraphEdge>,
    edges_by_type: FxHashMap<RelationshipType, FxHashMap<EdgeId, GraphEdge>>,
    edge_ids_by_node: FxHashMap<NodeId, FxHashSet<EdgeId>>,
    node_ids_by_file: FxHashMap<String, FxHashSet<NodeId>>,
    /// node_id → community_id (from MEMBER_OF edges).
    node_community: FxHashMap<NodeId, NodeId>,
    /// node_id → process_id (from STEP_IN_PROCESS edges).
    node_process: FxHashMap<NodeId, NodeId>,
    /// node_id → depth from entry-point files (via CONTAINS/DEFINES edges).
    /// 0 = entry-point file, higher = deeper in the containment tree.
    node_depth: FxHashMap<NodeId, i32>,
}

impl Default for KnowledgeGraph {
    fn default() -> Self { Self::new() }
}

impl KnowledgeGraph {
    pub fn new() -> Self {
        Self {
            node_map: FxHashMap::default(),
            edge_map: FxHashMap::default(),
            edges_by_type: FxHashMap::default(),
            edge_ids_by_node: FxHashMap::default(),
            node_ids_by_file: FxHashMap::default(),
            node_community: FxHashMap::default(),
            node_process: FxHashMap::default(),
            node_depth: FxHashMap::default(),
        }
    }

    // ── Node operations ────────────────────────────────────────────────

    pub fn add_node(&mut self, node: GraphNode) {
        if self.node_map.contains_key(&node.id) { return; }
        if let Some(fp) = node.properties.get("file_path") {
            self.node_ids_by_file
                .entry(fp.clone())
                .or_default()
                .insert(node.id.clone());
        }
        self.node_map.insert(node.id.clone(), node);
    }

    pub fn get_node(&self, id: &NodeId) -> Option<&GraphNode> {
        self.node_map.get(id)
    }

    pub fn has_node(&self, id: &NodeId) -> bool {
        self.node_map.contains_key(id)
    }

    pub fn node_count(&self) -> usize {
        self.node_map.len()
    }

    pub fn nodes(&self) -> impl Iterator<Item = &GraphNode> {
        self.node_map.values()
    }

    /// Remove a node and all edges touching it. O(edges-touching-node).
    pub fn remove_node(&mut self, id: &NodeId) -> bool {
        let Some(node) = self.node_map.remove(id) else { return false; };
        if let Some(fp) = node.properties.get("file_path") {
            if let Some(set) = self.node_ids_by_file.get_mut(fp) {
                set.remove(id);
                if set.is_empty() { self.node_ids_by_file.remove(fp); }
            }
        }
        if let Some(edge_ids) = self.edge_ids_by_node.remove(id) {
            for eid in &edge_ids {
                if let Some(edge) = self.edge_map.remove(eid) {
                    if let Some(bucket) = self.edges_by_type.get_mut(&edge.rel_type) {
                        bucket.remove(eid);
                        if bucket.is_empty() { self.edges_by_type.remove(&edge.rel_type); }
                    }
                    self.edge_ids_by_node.entry(edge.source_id.clone()).or_default().remove(eid);
                    if edge.target_id != edge.source_id {
                        self.edge_ids_by_node.entry(edge.target_id.clone()).or_default().remove(eid);
                    }
                }
            }
        }
        true
    }

    /// Remove all nodes (and their edges) belonging to a file. O(file-nodes × avg-edges).
    pub fn remove_nodes_by_file(&mut self, file_path: &str) -> usize {
        let Some(node_ids) = self.node_ids_by_file.remove(file_path) else { return 0; };
        let snapshot: Vec<NodeId> = node_ids.into_iter().collect();
        for id in &snapshot { self.remove_node(id); }
        snapshot.len()
    }

    pub fn nodes_by_file(&self, file_path: &str) -> Option<&FxHashSet<NodeId>> {
        self.node_ids_by_file.get(file_path)
    }

    // ── Edge operations ────────────────────────────────────────────────

    pub fn add_edge(&mut self, edge: GraphEdge) {
        if self.edge_map.contains_key(&edge.id) { return; }
        self.edge_ids_by_node.entry(edge.source_id.clone()).or_default().insert(edge.id.clone());
        if edge.target_id != edge.source_id {
            self.edge_ids_by_node.entry(edge.target_id.clone()).or_default().insert(edge.id.clone());
        }
        self.edges_by_type
            .entry(edge.rel_type)
            .or_default()
            .insert(edge.id.clone(), edge.clone());
        self.edge_map.insert(edge.id.clone(), edge);
    }

    pub fn get_edge(&self, id: &EdgeId) -> Option<&GraphEdge> {
        self.edge_map.get(id)
    }

    pub fn edge_count(&self) -> usize {
        self.edge_map.len()
    }

    pub fn edges(&self) -> impl Iterator<Item = &GraphEdge> {
        self.edge_map.values()
    }

    /// Iterate only edges of a given type. O(1) to get the bucket.
    pub fn edges_by_type(&self, rel_type: RelationshipType) -> impl Iterator<Item = &GraphEdge> {
        self.edges_by_type
            .get(&rel_type)
            .map(|bucket| bucket.values())
            .into_iter()
            .flatten()
    }

    /// Get all edge IDs touching a node (as source or target).
    pub fn edge_ids_for_node(&self, node_id: &NodeId) -> Option<&FxHashSet<EdgeId>> {
        self.edge_ids_by_node.get(node_id)
    }

    /// Get all edges touching a node.
    pub fn edges_for_node(&self, node_id: &NodeId) -> Vec<&GraphEdge> {
        self.edge_ids_by_node
            .get(node_id)
            .map(|ids| ids.iter().filter_map(|eid| self.edge_map.get(eid)).collect())
            .unwrap_or_default()
    }

    /// Get outgoing edges of a specific type from a node.
    pub fn outgoing_edges(&self, node_id: &NodeId, rel_type: RelationshipType) -> Vec<&GraphEdge> {
        self.edge_ids_by_node
            .get(node_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|eid| self.edge_map.get(eid))
                    .filter(|e| e.rel_type == rel_type && e.source_id == *node_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get incoming edges of a specific type to a node.
    pub fn incoming_edges(&self, node_id: &NodeId, rel_type: RelationshipType) -> Vec<&GraphEdge> {
        self.edge_ids_by_node
            .get(node_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|eid| self.edge_map.get(eid))
                    .filter(|e| e.rel_type == rel_type && e.target_id == *node_id)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn remove_edge(&mut self, id: &EdgeId) -> bool {
        let Some(edge) = self.edge_map.remove(id) else { return false; };
        if let Some(bucket) = self.edges_by_type.get_mut(&edge.rel_type) {
            bucket.remove(id);
            if bucket.is_empty() { self.edges_by_type.remove(&edge.rel_type); }
        }
        self.edge_ids_by_node.entry(edge.source_id.clone()).or_default().remove(id);
        if edge.target_id != edge.source_id {
            self.edge_ids_by_node.entry(edge.target_id.clone()).or_default().remove(id);
        }
        true
    }

    // ── Convenience ────────────────────────────────────────────────────

    /// Generate a deterministic edge ID from components.
    pub fn make_edge_id(source: &str, target: &str, rel_type: RelationshipType) -> EdgeId {
        format!("{}--{}--{}", source, rel_type.as_str(), target)
    }

    /// Clear all nodes and edges.
    pub fn clear(&mut self) {
        self.node_map.clear();
        self.edge_map.clear();
        self.edges_by_type.clear();
        self.edge_ids_by_node.clear();
        self.node_ids_by_file.clear();
        self.node_community.clear();
        self.node_process.clear();
        self.node_depth.clear();
    }

    /// Build a KnowledgeGraph from a GraphStore (SQLite-backed).
    ///
    /// Loads all symbols as nodes and all edges as relationships.
    /// Symbol IDs are converted from i64 (SQLite) to String (NodeId).
    pub fn from_store(store: &crate::store::GraphStore) -> rusqlite::Result<Self> {
        let mut graph = Self::new();

        // Load all files and create file nodes.
        let files = store.get_all_files()?;
        for file in &files {
            let file_node_id = format!("file:{}", file.id);
            let mut props = FxHashMap::default();
            props.insert("name".to_string(), file.path.clone());
            props.insert("file_path".to_string(), file.path.clone());
            props.insert("language".to_string(), file.language.clone());
            props.insert("kind".to_string(), "File".to_string());
            graph.add_node(GraphNode {
                id: file_node_id.clone(),
                label: "File".to_string(),
                properties: props,
            });
        }

        // Load all symbols as nodes.
        let symbols = store.get_all_symbols()?;
        for sym in &symbols {
            let node_id = format!("sym:{}", sym.id);
            let mut props = FxHashMap::default();
            props.insert("name".to_string(), sym.name.clone());
            props.insert("kind".to_string(), sym.kind.clone());
            props.insert("qualified_name".to_string(), sym.qualified_name.clone());
            props.insert("line".to_string(), sym.line.to_string());
            props.insert("col".to_string(), sym.col.to_string());
            if let Ok(Some(file)) = store.get_file_by_id(sym.file_id) {
                props.insert("file_path".to_string(), file.path.clone());
            }
            if let Some(scope_id) = sym.scope_id {
                props.insert("scope_id".to_string(), scope_id.to_string());
            }
            if let Some(owner_id) = sym.owner_symbol_id {
                props.insert("owner_symbol_id".to_string(), owner_id.to_string());
            }
            graph.add_node(GraphNode {
                id: node_id,
                label: sym.kind.clone(),
                properties: props,
            });
        }

        // Load all edges.
        let edges = store.get_all_edges()?;
        for edge in &edges {
            // Resolve node IDs: try sym: prefix first, then file: prefix.
            let sym_src = format!("sym:{}", edge.src_id);
            let sym_dst = format!("sym:{}", edge.dst_id);
            let file_src = format!("file:{}", edge.src_id);
            let file_dst = format!("file:{}", edge.dst_id);
            let src = if graph.has_node(&sym_src) { sym_src } else if graph.has_node(&file_src) { file_src } else { continue; };
            let dst = if graph.has_node(&sym_dst) { sym_dst } else if graph.has_node(&file_dst) { file_dst } else {
                // Community nodes (MEMBER_OF edges) have dst IDs in the 1_000_000+ range.
                if edge.edge_kind == "MEMBER_OF" && edge.dst_id >= 1_000_000 && edge.dst_id < 2_000_000 {
                    let community_id = format!("community_{}", edge.dst_id - 1_000_000);
                    let node_id = format!("com:{}", edge.dst_id);
                    if !graph.has_node(&node_id) {
                        let mut props = FxHashMap::default();
                        props.insert("name".to_string(), community_id.clone());
                        props.insert("kind".to_string(), "Community".to_string());
                        graph.add_node(GraphNode {
                            id: node_id.clone(),
                            label: "Community".to_string(),
                            properties: props,
                        });
                    }
                    node_id
                } else if edge.edge_kind == "STEP_IN_PROCESS" && edge.dst_id >= 2_000_000 {
                    // Process nodes have dst IDs in the 2_000_000+ range.
                    let node_id = format!("proc:{}", edge.dst_id);
                    if !graph.has_node(&node_id) {
                        let mut props = FxHashMap::default();
                        props.insert("name".to_string(), format!("process_{}", edge.dst_id - 2_000_000));
                        props.insert("kind".to_string(), "Process".to_string());
                        graph.add_node(GraphNode {
                            id: node_id.clone(),
                            label: "Process".to_string(),
                            properties: props,
                        });
                    }
                    node_id
                } else {
                    continue;
                }
            };
            let rel_type = match edge.edge_kind.as_str() {
                "CONTAINS" => RelationshipType::Contains,
                "CALLS" => RelationshipType::Calls,
                "INHERITS" => RelationshipType::Inherits,
                "IMPORTS" => RelationshipType::Imports,
                "DEFINES" => RelationshipType::Defines,
                "EXTENDS" => RelationshipType::Extends,
                "IMPLEMENTS" => RelationshipType::Implements,
                "ACCESSES" => RelationshipType::Accesses,
                "DECORATES" => RelationshipType::Decorates,
                "HAS_METHOD" => RelationshipType::HasMethod,
                "HAS_PROPERTY" => RelationshipType::HasProperty,
                "MEMBER_OF" => RelationshipType::MemberOf,
                "STEP_IN_PROCESS" => RelationshipType::StepInProcess,
                "HANDLES_ROUTE" => RelationshipType::HandlesRoute,
                "FETCHES" => RelationshipType::Fetches,
                "HANDLES_TOOL" => RelationshipType::HandlesTool,
                "ENTRY_POINT_OF" => RelationshipType::EntryPointOf,
                "USES" => RelationshipType::Uses,
                "METHOD_OVERRIDES" => RelationshipType::MethodOverrides,
                "METHOD_IMPLEMENTS" => RelationshipType::MethodImplements,
                "WRAPS" => RelationshipType::Wraps,
                "QUERIES" => RelationshipType::Queries,
                _ => continue,
            };
            // Track community memberships for the heuristic.
            if rel_type == RelationshipType::MemberOf {
                graph.node_community.insert(src.clone(), dst.clone());
            }
            // Track process participation for the heuristic.
            if rel_type == RelationshipType::StepInProcess {
                graph.node_process.insert(src.clone(), dst.clone());
            }

            graph.add_edge(GraphEdge {
                id: format!("{}--{:?}--{}", src, rel_type, dst),
                source_id: src,
                target_id: dst,
                rel_type,
                confidence: edge.confidence,
                reason: edge.edge_kind.clone(),
                step: None,
            });
        }

        // Compute depths from entry-point files along CONTAINS/DEFINES edges.
        graph.compute_depths();

        Ok(graph)
    }

    /// Get the community ID for a node, if any.
    pub fn node_community(&self, node_id: &NodeId) -> Option<&NodeId> {
        self.node_community.get(node_id)
    }

    /// Set the community ID for a node (used by tests and community detection).
    pub fn set_node_community(&mut self, node_id: NodeId, community_id: NodeId) {
        self.node_community.insert(node_id, community_id);
    }

    /// Get the process ID for a node, if any.
    pub fn node_process(&self, node_id: &NodeId) -> Option<&NodeId> {
        self.node_process.get(node_id)
    }

    /// Set the process ID for a node (used by tests).
    pub fn set_node_process(&mut self, node_id: NodeId, process_id: NodeId) {
        self.node_process.insert(node_id, process_id);
    }

    /// Get the depth of a node from entry-point files.
    /// Returns `None` if the node has no depth assigned.
    pub fn node_depth(&self, node_id: &NodeId) -> Option<i32> {
        self.node_depth.get(node_id).copied()
    }

    /// Set the depth for a node (used by tests).
    pub fn set_node_depth(&mut self, node_id: NodeId, depth: i32) {
        self.node_depth.insert(node_id, depth);
    }

    /// Compute depths for all nodes reachable from entry-point files.
    ///
    /// Entry-point files are file nodes with no incoming IMPORTS edges.
    /// BFS is run from entry points along CONTAINS and DEFINES edges,
    /// assigning depth = 0 to entry points, depth + 1 to their children, etc.
    ///
    /// This is used by the evidence path heuristic as a depth coherence signal.
    pub fn compute_depths(&mut self) {
        use std::collections::VecDeque;

        // Find entry-point files: file nodes with no incoming IMPORTS edges.
        let mut has_incoming_import: FxHashSet<NodeId> = FxHashSet::default();
        for edge in self.edges_by_type(RelationshipType::Imports) {
            has_incoming_import.insert(edge.target_id.clone());
        }

        // File nodes are those with "file:" prefix.
        let entry_files: Vec<NodeId> = self.node_map.keys()
            .filter(|nid| nid.starts_with("file:") && !has_incoming_import.contains(nid.as_str()))
            .cloned()
            .collect();

        let mut visited = FxHashSet::default();
        let mut q = VecDeque::new();

        for entry in &entry_files {
            self.node_depth.insert(entry.clone(), 0);
            visited.insert(entry.clone());
            q.push_back((entry.clone(), 0));
        }

        // BFS along CONTAINS and DEFINES edges.
        // Collect edges into vectors first to avoid borrow conflicts.
        while let Some((curr, curr_depth)) = q.pop_front() {
            let contains_edges: Vec<NodeId> = {
                self.outgoing_edges(&curr, RelationshipType::Contains)
                    .into_iter()
                    .map(|e| e.target_id.clone())
                    .collect()
            };
            let defines_edges: Vec<NodeId> = {
                self.outgoing_edges(&curr, RelationshipType::Defines)
                    .into_iter()
                    .map(|e| e.target_id.clone())
                    .collect()
            };

            for target in contains_edges {
                if !visited.contains(&target) {
                    visited.insert(target.clone());
                    self.node_depth.insert(target.clone(), curr_depth + 1);
                    q.push_back((target.clone(), curr_depth + 1));
                }
            }
            for target in defines_edges {
                if !visited.contains(&target) {
                    visited.insert(target.clone());
                    self.node_depth.insert(target.clone(), curr_depth + 1);
                    q.push_back((target.clone(), curr_depth + 1));
                }
            }
        }
    }

    /// Resolve a symbol node's display info.
    /// Returns (name, kind, file_path, line) for a `sym:<id>` node.
    pub fn resolve_symbol(&self, node_id: &str) -> Option<(&str, &str, &str, &str)> {
        let node = self.node_map.get(node_id)?;
        let name = node.properties.get("name")?.as_str();
        let kind = node.properties.get("kind")?.as_str();
        let file_path = node.properties.get("file_path")?.as_str();
        let line = node.properties.get("line")?.as_str();
        Some((name, kind, file_path, line))
    }

    /// Get the display name for any node (symbol, community, process, file).
    pub fn node_display_name(&self, node_id: &str) -> Option<&str> {
        self.node_map.get(node_id)?.properties.get("name").map(|s| s.as_str())
    }
}

// ── Graph layers (for EvidencePath traversal) ──────────────────────────────

/// Physical layer: File/Folder/Module containment.
pub fn physical_edges(graph: &KnowledgeGraph) -> Vec<&GraphEdge> {
    graph.edges_by_type(RelationshipType::Contains).collect()
}

/// Syntax layer: DEFINES, HAS_METHOD, HAS_PROPERTY.
pub fn syntax_edges(graph: &KnowledgeGraph) -> Vec<&GraphEdge> {
    graph.edges_by_type(RelationshipType::Defines)
        .chain(graph.edges_by_type(RelationshipType::HasMethod))
        .chain(graph.edges_by_type(RelationshipType::HasProperty))
        .collect()
}

/// Symbol layer: CALLS, INHERITS, IMPORTS, USES.
pub fn symbol_edges(graph: &KnowledgeGraph) -> Vec<&GraphEdge> {
    graph.edges_by_type(RelationshipType::Calls)
        .chain(graph.edges_by_type(RelationshipType::Inherits))
        .chain(graph.edges_by_type(RelationshipType::Imports))
        .chain(graph.edges_by_type(RelationshipType::Uses))
        .collect()
}

/// Semantic layer: ACCESSES, DECORATES, EXTENDS, IMPLEMENTS.
pub fn semantic_edges(graph: &KnowledgeGraph) -> Vec<&GraphEdge> {
    graph.edges_by_type(RelationshipType::Accesses)
        .chain(graph.edges_by_type(RelationshipType::Decorates))
        .chain(graph.edges_by_type(RelationshipType::Extends))
        .chain(graph.edges_by_type(RelationshipType::Implements))
        .collect()
}

/// Behavior layer: STEP_IN_PROCESS, ENTRY_POINT_OF.
pub fn behavior_edges(graph: &KnowledgeGraph) -> Vec<&GraphEdge> {
    graph.edges_by_type(RelationshipType::StepInProcess)
        .chain(graph.edges_by_type(RelationshipType::EntryPointOf))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node(id: &str, label: &str, file_path: &str) -> GraphNode {
        let mut props = FxHashMap::default();
        props.insert("file_path".to_string(), file_path.to_string());
        props.insert("name".to_string(), id.to_string());
        GraphNode { id: id.to_string(), label: label.to_string(), properties: props }
    }

    fn test_edge(src: &str, dst: &str, rel: RelationshipType) -> GraphEdge {
        GraphEdge {
            id: KnowledgeGraph::make_edge_id(src, dst, rel),
            source_id: src.to_string(),
            target_id: dst.to_string(),
            rel_type: rel,
            confidence: 1.0,
            reason: "test".to_string(),
            step: None,
        }
    }

    #[test]
    fn test_add_and_get_node() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        assert_eq!(g.node_count(), 1);
        assert!(g.has_node(&"n1".to_string()));
        assert_eq!(g.get_node(&"n1".to_string()).unwrap().label, "Function");
    }

    #[test]
    fn test_duplicate_node_ignored() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        g.add_node(test_node("n1", "Class", "src/lib.rs"));
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.get_node(&"n1".to_string()).unwrap().label, "Function");
    }

    #[test]
    fn test_add_and_get_edge() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        g.add_node(test_node("n2", "Function", "src/lib.rs"));
        g.add_edge(test_edge("n1", "n2", RelationshipType::Calls));
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.edges_by_type(RelationshipType::Calls).count(), 1);
    }

    #[test]
    fn test_remove_node_cascades_edges() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        g.add_node(test_node("n2", "Function", "src/lib.rs"));
        g.add_edge(test_edge("n1", "n2", RelationshipType::Calls));
        assert!(g.remove_node(&"n1".to_string()));
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn test_remove_nodes_by_file() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        g.add_node(test_node("n2", "Function", "src/lib.rs"));
        g.add_node(test_node("n3", "Function", "src/other.rs"));
        g.add_edge(test_edge("n1", "n2", RelationshipType::Calls));
        g.add_edge(test_edge("n2", "n3", RelationshipType::Calls));
        let removed = g.remove_nodes_by_file("src/lib.rs");
        assert_eq!(removed, 2);
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.edge_count(), 0);
    }

    #[test]
    fn test_outgoing_incoming_edges() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        g.add_node(test_node("n2", "Function", "src/lib.rs"));
        g.add_node(test_node("n3", "Function", "src/lib.rs"));
        g.add_edge(test_edge("n1", "n2", RelationshipType::Calls));
        g.add_edge(test_edge("n3", "n2", RelationshipType::Calls));
        assert_eq!(g.outgoing_edges(&"n1".to_string(), RelationshipType::Calls).len(), 1);
        assert_eq!(g.incoming_edges(&"n2".to_string(), RelationshipType::Calls).len(), 2);
    }

    #[test]
    fn test_edges_by_type_filter() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("n1", "Function", "src/lib.rs"));
        g.add_node(test_node("n2", "Function", "src/lib.rs"));
        g.add_node(test_node("n3", "Class", "src/lib.rs"));
        g.add_edge(test_edge("n1", "n2", RelationshipType::Calls));
        g.add_edge(test_edge("n3", "n2", RelationshipType::Contains));
        assert_eq!(g.edges_by_type(RelationshipType::Calls).count(), 1);
        assert_eq!(g.edges_by_type(RelationshipType::Contains).count(), 1);
        assert_eq!(g.edges_by_type(RelationshipType::Imports).count(), 0);
    }

    #[test]
    fn test_graph_layers() {
        let mut g = KnowledgeGraph::new();
        g.add_node(test_node("f1", "File", "src/lib.rs"));
        g.add_node(test_node("fn1", "Function", "src/lib.rs"));
        g.add_node(test_node("fn2", "Function", "src/lib.rs"));
        g.add_edge(test_edge("f1", "fn1", RelationshipType::Contains));
        g.add_edge(test_edge("f1", "fn2", RelationshipType::Defines));
        g.add_edge(test_edge("fn1", "fn2", RelationshipType::Calls));
        g.add_edge(test_edge("fn1", "fn2", RelationshipType::Accesses));
        g.add_edge(test_edge("fn1", "fn2", RelationshipType::StepInProcess));
        assert_eq!(physical_edges(&g).len(), 1);
        assert_eq!(syntax_edges(&g).len(), 1);
        assert_eq!(symbol_edges(&g).len(), 1);
        assert_eq!(semantic_edges(&g).len(), 1);
        assert_eq!(behavior_edges(&g).len(), 1);
    }

    #[test]
    fn test_compute_depths() {
        let mut g = KnowledgeGraph::new();
        // file:1 -> sym:1 -> sym:2 -> sym:3 (nested)
        g.add_node(GraphNode { id: "file:1".into(), label: "File".into(), properties: FxHashMap::default() });
        g.add_node(GraphNode { id: "sym:1".into(), label: "Function".into(), properties: FxHashMap::default() });
        g.add_node(GraphNode { id: "sym:2".into(), label: "Function".into(), properties: FxHashMap::default() });
        g.add_node(GraphNode { id: "sym:3".into(), label: "Function".into(), properties: FxHashMap::default() });
        g.add_edge(test_edge("file:1", "sym:1", RelationshipType::Contains));
        g.add_edge(test_edge("sym:1", "sym:2", RelationshipType::Contains));
        g.add_edge(test_edge("sym:2", "sym:3", RelationshipType::Contains));

        g.compute_depths();

        assert_eq!(g.node_depth(&"file:1".into()), Some(0));
        assert_eq!(g.node_depth(&"sym:1".into()), Some(1));
        assert_eq!(g.node_depth(&"sym:2".into()), Some(2));
        assert_eq!(g.node_depth(&"sym:3".into()), Some(3));
    }

    #[test]
    fn test_depth_coherence() {
        let mut g = KnowledgeGraph::new();
        g.add_node(GraphNode { id: "file:1".into(), label: "File".into(), properties: FxHashMap::default() });
        g.add_node(GraphNode { id: "sym:1".into(), label: "Function".into(), properties: FxHashMap::default() });
        g.add_node(GraphNode { id: "sym:2".into(), label: "Function".into(), properties: FxHashMap::default() });
        g.add_edge(test_edge("file:1", "sym:1", RelationshipType::Contains));
        g.add_edge(test_edge("file:1", "sym:2", RelationshipType::Contains));

        g.compute_depths();

        // Both sym:1 and sym:2 have depth 1 (within 2 of each other).
        let d1 = g.node_depth(&"sym:1".into());
        let d2 = g.node_depth(&"sym:2".into());
        assert!(d1.is_some() && d2.is_some());
        assert!((d1.unwrap() - d2.unwrap()).abs() <= 2);
    }
}
