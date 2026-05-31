//! EvidencePath — A* + beam traversal for code intelligence queries.
//!
//! EvidencePath is the killer feature of the code intelligence engine.
//! Given a query (natural language or symbol name), it:
//!
//! 1. Generates candidate seeds via hybrid search (BM25 + graph proximity)
//! 2. Traverses the graph using A* with beam search, following high-confidence edges
//! 3. Collects evidence packets (token-bounded) along the best paths
//! 4. Returns ranked evidence paths with confidence scores
//!
//! The traversal uses multiple graph layers (physical, syntax, symbol, semantic, behavior)
//! with layer-specific edge costs. The A* heuristic combines textual relevance with
//! graph distance.

use crate::graph::{
    KnowledgeGraph, NodeId, RelationshipType,
};
use crate::search::{hybrid_search, SearchConfig, SearchHit};
use crate::store::GraphStore;
use rustc_hash::FxHashSet;
use serde::{Serialize, Deserialize};

// ── Evidence types ──────────────────────────────────────────────────────────

/// A single piece of evidence collected during traversal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// The node this evidence points to.
    pub node_id: NodeId,
    /// Human-readable label.
    pub label: String,
    /// File path.
    pub file_path: String,
    /// Line number.
    pub line: usize,
    /// Relevance score (0.0 - 1.0).
    pub relevance: f64,
    /// How this evidence was found.
    pub via: EvidenceVia,
}

/// How evidence was discovered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceVia {
    /// Direct text match from search.
    TextMatch,
    /// Followed a CALLS edge.
    CallChain,
    /// Followed an INHERITS edge.
    Inheritance,
    /// Followed an IMPORTS edge.
    ImportChain,
    /// Followed an ACCESSES edge.
    DataFlow,
    /// Followed a CONTAINS edge (structural).
    Containment,
    /// Semantic similarity.
    Semantic,
}

/// A complete evidence path from query to answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidencePath {
    /// Ordered steps from seed to target.
    pub steps: Vec<Evidence>,
    /// Overall confidence score.
    pub confidence: f64,
    /// Total path cost (lower = better).
    pub cost: f64,
    /// Human-readable explanation.
    pub explanation: String,
    /// Per-step direction: true = forward (along edge direction), false = backward.
    pub directions: Vec<bool>,
    /// Quality signals for this path.
    pub quality: PathQuality,
}

/// Quality signals that explain WHY a path is good.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathQuality {
    /// Number of steps that follow an execution flow (process).
    pub process_steps: usize,
    /// Number of steps that stay within the same community.
    pub community_steps: usize,
    /// Number of steps that are forward (along edge direction) vs backward.
    pub forward_steps: usize,
    /// Number of steps that are backward (against edge direction).
    pub backward_steps: usize,
    /// The process ID if this path follows an execution flow.
    pub process_id: Option<String>,
}

/// Configuration for EvidencePath traversal.
pub struct EvidenceConfig {
    /// Max number of seed candidates to explore (default: 10).
    pub max_seeds: usize,
    /// Beam width for A* search (default: 5).
    pub beam_width: usize,
    /// Max traversal depth (default: 5).
    pub max_depth: usize,
    /// Max total evidence packets to collect (default: 20).
    pub max_evidence: usize,
    /// Token budget for evidence text (default: 4000).
    pub token_budget: usize,
    /// Edge cost for CALLS edges (default: 1.0).
    pub calls_cost: f64,
    /// Edge cost for INHERITS edges (default: 1.5).
    pub inherits_cost: f64,
    /// Edge cost for IMPORTS edges (default: 2.0).
    pub imports_cost: f64,
    /// Edge cost for ACCESSES edges (default: 1.8).
    pub accesses_cost: f64,
    /// Edge cost for CONTAINS edges (default: 3.0).
    pub contains_cost: f64,
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            max_seeds: 10,
            beam_width: 5,
            max_depth: 5,
            max_evidence: 20,
            token_budget: 4000,
            calls_cost: 1.0,
            inherits_cost: 1.5,
            imports_cost: 2.0,
            accesses_cost: 1.8,
            contains_cost: 3.0,
        }
    }
}

// ── A* search state ─────────────────────────────────────────────────────────

/// A node in the A* open set.
#[derive(Debug, Clone)]
struct SearchState {
    node_id: NodeId,
    g: f64,
    /// Heuristic estimate of remaining cost to a relevant target.
    h: f64,
    path: Vec<NodeId>,
    evidence: Vec<Evidence>,
    /// Per-step direction: true = forward (along edge), false = backward.
    directions: Vec<bool>,
    /// Accumulated quality signals.
    quality: PathQuality,
}

impl SearchState {
    fn new(node_id: NodeId, g: f64, h: f64, path: Vec<NodeId>) -> Self {
        Self { node_id, g, h, path, evidence: Vec::new(), directions: Vec::new(), quality: PathQuality::default() }
    }

    fn f_score(&self) -> f64 {
        self.g + self.h
    }
}

// ── EvidencePath engine ─────────────────────────────────────────────────────

/// Find evidence paths for a query using A* + beam search.
pub fn find_evidence_paths(
    store: &GraphStore,
    graph: &KnowledgeGraph,
    query: &str,
    config: &EvidenceConfig,
) -> Vec<EvidencePath> {
    // Step 1: Get seed candidates from hybrid search (BM25 + graph proximity).
    let search_config = SearchConfig {
        limit: config.max_seeds,
        ..Default::default()
    };
    let seeds = match hybrid_search(store, query, &search_config) {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(query = %query, error = %e, "Evidence seed search failed");
            return Vec::new();
        }
    };

    if seeds.is_empty() {
        return Vec::new();
    }

    // Step 2: For each seed, run A* beam search.
    // Extract query terms for the heuristic function.
    let query_terms: Vec<String> = query.split_whitespace()
        .map(String::from)
        .filter(|s| s.len() >= 2)
        .collect();

    let mut all_paths: Vec<EvidencePath> = Vec::new();

    for seed in &seeds {
        let paths = astar_beam(graph, seed, config, &query_terms);
        all_paths.extend(paths);
    }

    // Step 3: Deduplicate and rank.
    all_paths.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    all_paths.truncate(config.max_evidence);

    all_paths
}

/// Find the best path between two specific symbol IDs using A* beam search.
///
/// Unlike `find_evidence_paths` (which searches from seeds to any relevant
/// target), this targets a specific destination node. The heuristic is
/// adjusted to estimate distance to the target rather than to "any relevant"
/// node.
///
/// Returns evidence paths from `from_id` to `to_id`, ranked by confidence.
/// The `from_id` and `to_id` should be raw SQLite symbol IDs (i64); they
/// will be prefixed with "sym:" internally.
pub fn find_path_between(
    _store: &GraphStore,
    graph: &KnowledgeGraph,
    from_id: i64,
    to_id: i64,
    config: &EvidenceConfig,
) -> Vec<EvidencePath> {
    let from_node_id = format!("sym:{}", from_id);
    let to_node_id = format!("sym:{}", to_id);

    if !graph.has_node(&from_node_id) || !graph.has_node(&to_node_id) {
        return Vec::new();
    }

    // Get the target node's name for text relevance scoring.
    let target_name = graph.get_node(&to_node_id)
        .and_then(|n| n.properties.get("name").cloned())
        .unwrap_or_default();
    let target_terms: Vec<String> = target_name.split_whitespace()
        .map(String::from)
        .filter(|s| s.len() >= 2)
        .collect();
    let query_terms = if target_terms.is_empty() {
        vec![target_name]
    } else {
        target_terms
    };

    // Create a synthetic SearchHit for the source.
    let source_name = graph.get_node(&from_node_id)
        .and_then(|n| n.properties.get("name").cloned())
        .unwrap_or_default();
    let source_file = graph.get_node(&from_node_id)
        .and_then(|n| n.properties.get("file_path").cloned())
        .unwrap_or_default();
    let source_line = graph.get_node(&from_node_id)
        .and_then(|n| n.properties.get("line").and_then(|l| l.parse().ok()))
        .unwrap_or(0);

    let seed = SearchHit {
        node_id: from_id,
        name: source_name,
        kind: String::new(),
        file_path: source_file,
        line: source_line,
        score: 1.0,
        matched_text: String::new(),
    };

    // Override the heuristic to target the specific destination.
    // We do this by running a modified A* that uses the target node
    // as the heuristic target.
    astar_beam_targeted(graph, &seed, config, &query_terms, &to_node_id)
}

/// A* beam search from a seed node, targeting a specific destination.
///
/// The heuristic uses the target node for community/process coherence
/// and text relevance against the target's name, making the search
/// converge toward the destination.
fn astar_beam_targeted(
    graph: &KnowledgeGraph,
    seed: &SearchHit,
    config: &EvidenceConfig,
    query_terms: &[String],
    target_id: &NodeId,
) -> Vec<EvidencePath> {
    let mut results: Vec<EvidencePath> = Vec::new();
    let mut visited: FxHashSet<NodeId> = FxHashSet::default();

    let seed_id = format!("sym:{}", seed.node_id);
    let seed_h = node_heuristic(graph, &seed_id, seed.score, query_terms, target_id);
    let initial = SearchState::new(seed_id.clone(), 0.0, seed_h, vec![seed_id.clone()]);
    let mut beam: Vec<SearchState> = vec![initial];

    visited.insert(seed_id.clone());

    // Add seed evidence.
    if let Some(node) = graph.get_node(&seed_id) {
        let label = node.properties.get("name")
            .cloned()
            .unwrap_or_else(|| seed.name.clone());
        let file_path = node.properties.get("file_path")
            .cloned()
            .unwrap_or_else(|| seed.file_path.clone());
        let line = node.properties.get("line")
            .and_then(|l| l.parse().ok())
            .unwrap_or(seed.line);
        results.push(EvidencePath {
            steps: vec![Evidence {
                node_id: seed_id.clone(),
                label,
                file_path,
                line,
                relevance: seed.score,
                via: EvidenceVia::TextMatch,
            }],
            confidence: seed.score.min(1.0),
            cost: 0.0,
            explanation: format!("Start: {}", seed.name),
            directions: vec![],
            quality: PathQuality::default(),
        });
    }

    for _depth in 0..config.max_depth {
        let mut next_beam: Vec<SearchState> = Vec::new();

        for state in &beam {
            // Follow outgoing edges (forward direction).
            let outgoing = graph.outgoing_edges(&state.node_id, RelationshipType::Calls)
                .into_iter()
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Inherits))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Extends))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Implements))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::MethodOverrides))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::MethodImplements))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Imports))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Uses))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Accesses))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Defines))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Contains));

            let incoming: Vec<_> = graph.incoming_edges(&state.node_id, RelationshipType::Calls)
                .into_iter()
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Inherits))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Extends))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Implements))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::MethodOverrides))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::MethodImplements))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Imports))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Uses))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Accesses))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Defines))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Contains))
                .collect();

            // Process outgoing edges (forward direction).
            for edge in outgoing {
                if visited.contains(&edge.target_id) { continue; }

                let edge_cost_val = edge_cost(edge.rel_type, config);
                let new_g = state.g + edge_cost_val;
                let mut new_path = state.path.clone();
                new_path.push(edge.target_id.clone());

                let h = node_heuristic(graph, &edge.target_id, 0.0, query_terms, target_id);

                let mut new_quality = state.quality.clone();
                new_quality.forward_steps += 1;
                if graph.node_process(&edge.target_id).is_some()
                    && graph.node_process(&edge.target_id) == graph.node_process(&state.node_id)
                {
                    new_quality.process_steps += 1;
                    new_quality.process_id = graph.node_process(&edge.target_id).cloned();
                }
                if graph.node_community(&edge.target_id).is_some()
                    && graph.node_community(&edge.target_id) == graph.node_community(&state.node_id)
                {
                    new_quality.community_steps += 1;
                }

                let mut new_state = SearchState::new(
                    edge.target_id.clone(),
                    new_g,
                    h,
                    new_path,
                );
                new_state.quality = new_quality;
                new_state.directions = state.directions.clone();
                new_state.directions.push(true);

                if let Some(node) = graph.get_node(&edge.target_id) {
                    let via = match edge.rel_type {
                        RelationshipType::Calls => EvidenceVia::CallChain,
                        RelationshipType::Inherits | RelationshipType::Extends | RelationshipType::Implements | RelationshipType::MethodOverrides | RelationshipType::MethodImplements => EvidenceVia::Inheritance,
                        RelationshipType::Imports | RelationshipType::Uses => EvidenceVia::ImportChain,
                        RelationshipType::Accesses => EvidenceVia::DataFlow,
                        RelationshipType::Contains | RelationshipType::Defines => EvidenceVia::Containment,
                        _ => EvidenceVia::Semantic,
                    };

                    let label = node.properties.get("name")
                        .cloned()
                        .unwrap_or_else(|| node.label.clone());
                    let file_path = node.properties.get("file_path")
                        .cloned()
                        .unwrap_or_default();
                    let line = node.properties.get("line")
                        .and_then(|l| l.parse().ok())
                        .unwrap_or(0);

                    new_state.evidence.push(Evidence {
                        node_id: edge.target_id.clone(),
                        label,
                        file_path,
                        line,
                        relevance: edge.confidence,
                        via,
                    });
                }

                // Check if we reached the target.
                if edge.target_id == *target_id {
                    let confidence = 1.0 / (1.0 + new_state.g);
                    let explanation = build_path_explanation(&new_state.quality, new_state.path.len() - 1, new_state.g);
                    results.push(EvidencePath {
                        steps: new_state.evidence.clone(),
                        confidence,
                        cost: new_state.g,
                        explanation,
                        directions: new_state.directions.clone(),
                        quality: new_state.quality.clone(),
                    });
                }

                next_beam.push(new_state);
                visited.insert(edge.target_id.clone());
            }

            // Process incoming edges (reverse direction).
            for edge in incoming {
                if visited.contains(&edge.source_id) { continue; }

                let edge_cost_val = edge_cost(edge.rel_type, config) + 0.5;
                let new_g = state.g + edge_cost_val;
                let mut new_path = state.path.clone();
                new_path.push(edge.source_id.clone());

                let h = node_heuristic(graph, &edge.source_id, 0.0, query_terms, target_id);

                let mut new_quality = state.quality.clone();
                new_quality.backward_steps += 1;
                if graph.node_process(&edge.source_id).is_some()
                    && graph.node_process(&edge.source_id) == graph.node_process(&state.node_id)
                {
                    new_quality.process_steps += 1;
                    new_quality.process_id = graph.node_process(&edge.source_id).cloned();
                }
                if graph.node_community(&edge.source_id).is_some()
                    && graph.node_community(&edge.source_id) == graph.node_community(&state.node_id)
                {
                    new_quality.community_steps += 1;
                }

                let mut new_state = SearchState::new(
                    edge.source_id.clone(),
                    new_g,
                    h,
                    new_path,
                );
                new_state.quality = new_quality;
                new_state.directions = state.directions.clone();
                new_state.directions.push(false);

                if let Some(node) = graph.get_node(&edge.source_id) {
                    let via = match edge.rel_type {
                        RelationshipType::Calls => EvidenceVia::CallChain,
                        RelationshipType::Inherits | RelationshipType::Extends | RelationshipType::Implements | RelationshipType::MethodOverrides | RelationshipType::MethodImplements => EvidenceVia::Inheritance,
                        RelationshipType::Imports | RelationshipType::Uses => EvidenceVia::ImportChain,
                        RelationshipType::Accesses => EvidenceVia::DataFlow,
                        RelationshipType::Contains | RelationshipType::Defines => EvidenceVia::Containment,
                        _ => EvidenceVia::Semantic,
                    };

                    let label = node.properties.get("name")
                        .cloned()
                        .unwrap_or_else(|| node.label.clone());
                    let file_path = node.properties.get("file_path")
                        .cloned()
                        .unwrap_or_default();
                    let line = node.properties.get("line")
                        .and_then(|l| l.parse().ok())
                        .unwrap_or(0);

                    new_state.evidence.push(Evidence {
                        node_id: edge.source_id.clone(),
                        label,
                        file_path,
                        line,
                        relevance: edge.confidence * 0.8,
                        via,
                    });
                }

                // Check if we reached the target via incoming edge.
                if edge.source_id == *target_id {
                    let confidence = 1.0 / (1.0 + new_state.g);
                    let explanation = build_path_explanation(&new_state.quality, new_state.path.len() - 1, new_state.g);
                    results.push(EvidencePath {
                        steps: new_state.evidence.clone(),
                        confidence,
                        cost: new_state.g,
                        explanation,
                        directions: new_state.directions.clone(),
                        quality: new_state.quality.clone(),
                    });
                }

                next_beam.push(new_state);
                visited.insert(edge.source_id.clone());
            }
        }

        if next_beam.is_empty() { break; }

        next_beam.sort_by(|a, b| a.f_score().partial_cmp(&b.f_score()).unwrap_or(std::cmp::Ordering::Equal));
        next_beam.truncate(config.beam_width);

        // Collect non-target paths too (partial progress).
        for state in &next_beam {
            if !state.evidence.is_empty() {
                let confidence = 1.0 / (1.0 + state.g);
                let explanation = build_path_explanation(&state.quality, state.path.len() - 1, state.g);
                results.push(EvidencePath {
                    steps: state.evidence.clone(),
                    confidence,
                    cost: state.g,
                    explanation,
                    directions: state.directions.clone(),
                    quality: state.quality.clone(),
                });
            }
        }

        beam = next_beam;
    }

    // Sort: paths that reach the target first (by confidence), then partial paths.
    results.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(config.max_evidence);
    results
}

/// A* beam search from a seed node.
///
/// Heuristic: `h = (1.0 - combined_relevance) * min_edge_cost`
///
/// `combined_relevance` blends:
/// - Text relevance: name/qualified_name match against query terms (0.0–1.0)
/// - Community coherence: bonus if current and target share a community (0.0–0.3)
///
/// The heuristic remains admissible because the community bonus is capped
/// at 0.3 (never makes h negative) and `min_edge_cost` is the cheapest edge.
///
/// Traversal follows:
/// - All outgoing edges (CALLS, INHERITS, IMPORTS, ACCESSES, etc.)
/// - All incoming edges of every type (who calls this, who imports this, etc.)
///   with a direction penalty to prefer forward traversal.
fn astar_beam(graph: &KnowledgeGraph, seed: &SearchHit, config: &EvidenceConfig, query_terms: &[String]) -> Vec<EvidencePath> {
    let mut results: Vec<EvidencePath> = Vec::new();
    let mut visited: FxHashSet<NodeId> = FxHashSet::default();

    // SearchHit.node_id is a raw SQLite symbol ID (i64).
    // KnowledgeGraph stores symbol nodes with "sym:" prefix.
    let seed_id = format!("sym:{}", seed.node_id);
    let seed_h = node_heuristic(graph, &seed_id, seed.score, query_terms, &seed_id);
    let initial = SearchState::new(seed_id.clone(), 0.0, seed_h, vec![seed_id.clone()]);
    let mut beam: Vec<SearchState> = vec![initial];

    visited.insert(seed_id.clone());

    // Add seed evidence.
    if let Some(node) = graph.get_node(&seed_id) {
        let label = node.properties.get("name")
            .cloned()
            .unwrap_or_else(|| seed.name.clone());
        let file_path = node.properties.get("file_path")
            .cloned()
            .unwrap_or_else(|| seed.file_path.clone());
        let line = node.properties.get("line")
            .and_then(|l| l.parse().ok())
            .unwrap_or(seed.line);
        results.push(EvidencePath {
            steps: vec![Evidence {
                node_id: seed_id.clone(),
                label,
                file_path,
                line,
                relevance: seed.score,
                via: EvidenceVia::TextMatch,
            }],
            confidence: seed.score.min(1.0),
            cost: 0.0,
            explanation: format!("Direct match for '{}'", seed.name),
            directions: vec![],
            quality: PathQuality::default(),
        });
    }

    for _depth in 0..config.max_depth {
        let mut next_beam: Vec<SearchState> = Vec::new();

        for state in &beam {
            // Follow outgoing edges: the current node is the source.
            let outgoing = graph.outgoing_edges(&state.node_id, RelationshipType::Calls)
                .into_iter()
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Inherits))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Extends))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Implements))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::MethodOverrides))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::MethodImplements))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Imports))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Uses))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Accesses))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Defines))
                .chain(graph.outgoing_edges(&state.node_id, RelationshipType::Contains));

            // Follow incoming edges of ALL types (callers, importers, etc.) —
            // useful for "who depends on this?" context. Each gets a direction
            // penalty since they go "backward" relative to the edge direction.
            let incoming: Vec<_> = graph.incoming_edges(&state.node_id, RelationshipType::Calls)
                .into_iter()
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Inherits))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Extends))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Implements))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::MethodOverrides))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::MethodImplements))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Imports))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Uses))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Accesses))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Defines))
                .chain(graph.incoming_edges(&state.node_id, RelationshipType::Contains))
                .collect::<Vec<_>>();

            // Process outgoing edges (forward direction).
            for edge in outgoing {
                if visited.contains(&edge.target_id) { continue; }

                let edge_cost_val = edge_cost(edge.rel_type, config);
                let new_g = state.g + edge_cost_val;
                let mut new_path = state.path.clone();
                new_path.push(edge.target_id.clone());

                let h = node_heuristic(graph, &edge.target_id, 0.0, query_terms, &seed_id);

                // Inherit quality from parent and extend.
                let mut new_quality = state.quality.clone();
                new_quality.forward_steps += 1;
                if graph.node_process(&edge.target_id).is_some() && graph.node_process(&edge.target_id) == graph.node_process(&state.node_id) {
                    new_quality.process_steps += 1;
                    new_quality.process_id = graph.node_process(&edge.target_id).cloned();
                }
                if graph.node_community(&edge.target_id).is_some() && graph.node_community(&edge.target_id) == graph.node_community(&state.node_id) {
                    new_quality.community_steps += 1;
                }

                let mut new_state = SearchState::new(
                    edge.target_id.clone(),
                    new_g,
                    h,
                    new_path,
                );
                new_state.quality = new_quality;
                new_state.directions = state.directions.clone();
                new_state.directions.push(true); // forward

                if let Some(node) = graph.get_node(&edge.target_id) {
                    let via = match edge.rel_type {
                        RelationshipType::Calls => EvidenceVia::CallChain,
                        RelationshipType::Inherits | RelationshipType::Extends | RelationshipType::Implements | RelationshipType::MethodOverrides | RelationshipType::MethodImplements => EvidenceVia::Inheritance,
                        RelationshipType::Imports | RelationshipType::Uses => EvidenceVia::ImportChain,
                        RelationshipType::Accesses => EvidenceVia::DataFlow,
                        RelationshipType::Contains | RelationshipType::Defines => EvidenceVia::Containment,
                        _ => EvidenceVia::Semantic,
                    };

                    let label = node.properties.get("name")
                        .cloned()
                        .unwrap_or_else(|| node.label.clone());
                    let file_path = node.properties.get("file_path")
                        .cloned()
                        .unwrap_or_default();
                    let line = node.properties.get("line")
                        .and_then(|l| l.parse().ok())
                        .unwrap_or(0);

                    new_state.evidence.push(Evidence {
                        node_id: edge.target_id.clone(),
                        label,
                        file_path,
                        line,
                        relevance: edge.confidence,
                        via,
                    });
                }

                next_beam.push(new_state);
                visited.insert(edge.target_id.clone());
            }

            // Process incoming edges (reverse direction) — these get a small
            // penalty since they go "backward" relative to the edge direction.
            for edge in incoming {
                if visited.contains(&edge.source_id) { continue; }

                let edge_cost_val = edge_cost(edge.rel_type, config) + 0.5; // direction penalty
                let new_g = state.g + edge_cost_val;
                let mut new_path = state.path.clone();
                new_path.push(edge.source_id.clone());

                let h = node_heuristic(graph, &edge.source_id, 0.0, query_terms, &seed_id);

                // Inherit quality from parent and extend.
                let mut new_quality = state.quality.clone();
                new_quality.backward_steps += 1;
                if graph.node_process(&edge.source_id).is_some() && graph.node_process(&edge.source_id) == graph.node_process(&state.node_id) {
                    new_quality.process_steps += 1;
                    new_quality.process_id = graph.node_process(&edge.source_id).cloned();
                }
                if graph.node_community(&edge.source_id).is_some() && graph.node_community(&edge.source_id) == graph.node_community(&state.node_id) {
                    new_quality.community_steps += 1;
                }

                let mut new_state = SearchState::new(
                    edge.source_id.clone(),
                    new_g,
                    h,
                    new_path,
                );
                new_state.quality = new_quality;
                new_state.directions = state.directions.clone();
                new_state.directions.push(false); // backward

                if let Some(node) = graph.get_node(&edge.source_id) {
                    let via = match edge.rel_type {
                        RelationshipType::Calls => EvidenceVia::CallChain,
                        RelationshipType::Inherits | RelationshipType::Extends | RelationshipType::Implements | RelationshipType::MethodOverrides | RelationshipType::MethodImplements => EvidenceVia::Inheritance,
                        RelationshipType::Imports | RelationshipType::Uses => EvidenceVia::ImportChain,
                        RelationshipType::Accesses => EvidenceVia::DataFlow,
                        RelationshipType::Contains | RelationshipType::Defines => EvidenceVia::Containment,
                        _ => EvidenceVia::Semantic,
                    };

                    let label = node.properties.get("name")
                        .cloned()
                        .unwrap_or_else(|| node.label.clone());
                    let file_path = node.properties.get("file_path")
                        .cloned()
                        .unwrap_or_default();
                    let line = node.properties.get("line")
                        .and_then(|l| l.parse().ok())
                        .unwrap_or(0);

                    new_state.evidence.push(Evidence {
                        node_id: edge.source_id.clone(),
                        label,
                        file_path,
                        line,
                        relevance: edge.confidence * 0.8, // slight downweight for backward
                        via,
                    });
                }

                next_beam.push(new_state);
                visited.insert(edge.source_id.clone());
            }
        }

        if next_beam.is_empty() { break; }

        // Keep only the best `beam_width` states by f = g + h.
        next_beam.sort_by(|a, b| a.f_score().partial_cmp(&b.f_score()).unwrap_or(std::cmp::Ordering::Equal));
        next_beam.truncate(config.beam_width);

        // Collect evidence paths from beam.
        for state in &next_beam {
            if !state.evidence.is_empty() {
                let confidence = 1.0 / (1.0 + state.g);
                let explanation = build_path_explanation(&state.quality, state.path.len() - 1, state.g);
                results.push(EvidencePath {
                    steps: state.evidence.clone(),
                    confidence,
                    cost: state.g,
                    explanation,
                    directions: state.directions.clone(),
                    quality: state.quality.clone(),
                });
            }
        }

        beam = next_beam;
    }

    results
}

/// Build a human-readable explanation of path quality.
fn build_path_explanation(quality: &PathQuality, hops: usize, cost: f64) -> String {
    let mut parts = Vec::new();
    parts.push(format!("{} hops, cost {:.2}", hops, cost));

    if quality.process_steps > 0 {
        parts.push(format!("follows execution flow ({} steps)", quality.process_steps));
    }
    if quality.community_steps > 0 {
        parts.push(format!("same community ({} steps)", quality.community_steps));
    }
    if quality.backward_steps > 0 {
        parts.push(format!("{} backward", quality.backward_steps));
    }

    parts.join(" · ")
}

/// Admissible A* heuristic: estimates remaining cost from `node_id` to the
/// nearest query-relevant symbol.
///
/// `h = (1.0 - combined_relevance) * min_edge_cost`
///
/// `combined_relevance` = `text_relevance` + `process_bonus` + `community_bonus` + `depth_bonus`
/// - text_relevance: 0.0–1.0 from name/query term overlap (or FTS score)
/// - process_bonus: 0.0–0.4 if current node shares an execution flow with the target
/// - community_bonus: 0.0–0.3 if current node shares a community with the target
/// - depth_bonus: 0.0–0.15 if both nodes are at similar depth from entry points (within 2 levels)
///
/// Process bonus is highest because sharing an execution flow is a strong functional signal.
/// Depth bonus is lowest because it's an architectural-layer signal, not a semantic one.
/// Total bonus capped at 0.85 so combined_relevance never exceeds 1.0, keeping the heuristic admissible.
fn node_heuristic(graph: &KnowledgeGraph, node_id: &NodeId, fts_score: f64, query_terms: &[String], target_id: &NodeId) -> f64 {
    let text_rel = if fts_score > 0.0 {
        fts_score.min(1.0)
    } else {
        text_relevance(graph, node_id, query_terms)
    };
    let process_bonus = process_coherence(graph, node_id, target_id);
    let community_bonus = community_coherence(graph, node_id, target_id);
    let depth_bonus = depth_coherence(graph, node_id, target_id);
    let combined = (text_rel + process_bonus + community_bonus + depth_bonus).min(1.0);
    (1.0 - combined) * 1.0 // min_edge_cost = 1.0 (CALLS)
}

/// Process coherence bonus: 0.4 if both nodes participate in the same execution
/// flow (process), 0.0 otherwise. This is the strongest structural signal because
/// it means the nodes are actually connected by a runtime call path.
fn process_coherence(graph: &KnowledgeGraph, node_a: &NodeId, node_b: &NodeId) -> f64 {
    match (graph.node_process(node_a), graph.node_process(node_b)) {
        (Some(proc_a), Some(proc_b)) if proc_a == proc_b => 0.4,
        _ => 0.0,
    }
}

/// Community coherence bonus: 0.3 if both nodes are in the same community,
/// 0.0 otherwise.
fn community_coherence(graph: &KnowledgeGraph, node_a: &NodeId, node_b: &NodeId) -> f64 {
    match (graph.node_community(node_a), graph.node_community(node_b)) {
        (Some(com_a), Some(com_b)) if com_a == com_b => 0.3,
        _ => 0.0,
    }
}

/// Depth coherence bonus: 0.15 if both nodes are at similar depth from entry
/// points (within 2 levels). This rewards paths that stay at similar
/// architectural layers (e.g., both in service-layer code, not crossing from
/// entry-point handlers into deep utility code).
fn depth_coherence(graph: &KnowledgeGraph, node_a: &NodeId, node_b: &NodeId) -> f64 {
    match (graph.node_depth(node_a), graph.node_depth(node_b)) {
        (Some(da), Some(db)) if (da - db).abs() <= 2 => 0.15,
        _ => 0.0,
    }
}

/// Compute textual relevance of a node to the query terms.
/// Returns 0.0 (no match) to 1.0 (perfect match).
///
/// Scoring:
/// - Exact name match: 1.0
/// - Word-boundary name match: 0.7
/// - Word-boundary qualified name match: 0.4
/// - Primary synonym match in name: 0.5
/// - Secondary synonym match in name: 0.35
/// - Synonym match in qualified name: 0.25
/// - No match: 0.0
///
/// Word-boundary matching prevents "batch" from matching "auth" — the `contains`
/// check requires the term to appear at a word boundary (underscore, camelCase
/// transition, or string edge).
///
/// Synonym expansion allows "auth" to match "login", "token", "credential",
/// "session", "password", "jwt", "oauth", "permission", "role", "verify",
/// "authorize", "authenticate", "signup", "signin", etc.
fn text_relevance(graph: &KnowledgeGraph, node_id: &NodeId, query_terms: &[String]) -> f64 {
    let Some(node) = graph.get_node(node_id) else { return 0.0 };
    // Keep original-case names for camelCase boundary detection.
    let name_orig = node.properties.get("name").cloned().unwrap_or_default();
    let qual_orig = node.properties.get("qualified_name").cloned().unwrap_or_default();
    // Lowercased versions for exact/equality checks.
    let name_lower = name_orig.to_lowercase();
    let _qual_lower = qual_orig.to_lowercase();

    let mut best = 0.0_f64;
    for term in query_terms {
        let term_lower = term.to_lowercase();
        // Direct matches: equality on lowercased, word-boundary on original.
        if name_lower == term_lower {
            best = best.max(1.0);
        } else if word_boundary_match(&name_orig, term) {
            best = best.max(0.7);
        } else if word_boundary_match(&qual_orig, term) {
            best = best.max(0.4);
        } else {
            // Synonym expansion: check if the node name matches any synonym
            // of the query term. Primary synonyms score higher than secondary.
            let (primary, secondary) = expand_synonyms(&term_lower);
            let mut found = false;
            for syn in &primary {
                if name_lower == *syn || word_boundary_match(&name_orig, syn) {
                    best = best.max(0.5);
                    found = true;
                    break;
                }
            }
            if !found {
                for syn in &primary {
                    if word_boundary_match(&qual_orig, syn) {
                        best = best.max(0.25);
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                for syn in &secondary {
                    if name_lower == *syn || word_boundary_match(&name_orig, syn) {
                        best = best.max(0.35);
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                for syn in &secondary {
                    if word_boundary_match(&qual_orig, syn) {
                        best = best.max(0.25);
                        break;
                    }
                }
            }
        }
    }
    best
}

/// Check if `needle` appears in `haystack` at a word boundary.
///
/// Word boundaries in code identifiers are:
/// - Start/end of string
/// - Underscores (snake_case)
/// - CamelCase transitions (lowercase→uppercase)
/// - `::` module separators (Rust, Java, C++)
/// - `.` property separators (JS, TS, Python)
/// - `-` kebab-case separators
///
/// This prevents "batch" from matching "auth" while allowing
/// "authorize" to match "auth" (start-of-word boundary) and
/// "auth::login" to match "auth" (module separator boundary).
fn word_boundary_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() { return false; }
    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let hay_bytes = haystack.as_bytes();
    let needle_len = needle_lower.len();
    let mut search_start = 0;
    while let Some(pos) = hay_lower[search_start..].find(&needle_lower) {
        let abs_pos = search_start + pos;
        let before = abs_pos.checked_sub(1).and_then(|i| hay_bytes.get(i).copied());
        let after = hay_bytes.get(abs_pos + needle_len).copied();

        let before_ok = match before {
            None => true,
            Some(b'_') => true,
            Some(b':') => true,
            Some(b'.') => true,
            Some(b'-') => true,
            Some(b) if b.is_ascii_lowercase() && hay_bytes[abs_pos].is_ascii_uppercase() => true,
            _ => false,
        };
        let after_ok = match after {
            None => true,
            Some(b'_') => true,
            Some(b':') => true,
            Some(b'.') => true,
            Some(b'-') => true,
            Some(b) if b.is_ascii_uppercase() && hay_bytes[abs_pos + needle_len - 1].is_ascii_lowercase() => true,
            _ => false,
        };

        if before_ok && after_ok {
            return true;
        }
        search_start = abs_pos + 1;
        if search_start + needle_len > hay_lower.len() {
            break;
        }
    }
    false
}

/// Expand a query term into primary and secondary synonym sets.
///
/// Primary synonyms are the most conceptually central terms (score 0.5 in name).
/// Secondary synonyms are more peripheral (score 0.35 in name).
/// This is a lightweight, static synonym map — no embeddings needed.
/// Covers the most common code intelligence query domains:
/// auth, db/database, http, api, test, config, error, log, cache, crypto.
fn expand_synonyms(term: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    match term {
        // ── Authentication / authorization ──────────────────────────────
        "auth" | "authentication" | "authorization" | "authorize" | "authenticate" => (
            vec!["login", "logout", "signin", "signout", "signup", "register",
                 "token", "jwt", "oauth", "credential", "password", "session"],
            vec!["cookie", "csrf", "xsrf", "permission", "role", "acl",
                 "verify", "validate", "forgot", "reset", "mfa", "2fa",
                 "bearer", "apikey", "api_key", "secret", "hash", "saml",
                 "passwd", "pwd", "totp", "otp", "access"],
        ),
        "login" | "signin" => (
            vec!["auth", "authenticate", "session", "token", "credential", "password"],
            vec!["logout", "signout", "signup", "register"],
        ),
        "token" | "jwt" => (
            vec!["auth", "bearer", "session", "cookie", "refresh", "verify"],
            vec!["decode", "encode", "sign", "secret", "credential"],
        ),
        "credential" | "password" | "passwd" | "pwd" => (
            vec!["auth", "login", "hash", "encrypt", "salt", "verify"],
            vec!["validate", "reset", "forgot", "change"],
        ),
        "session" => (
            vec!["auth", "login", "token", "cookie"],
            vec!["store", "redis", "cache", "expire", "timeout", "destroy", "logout"],
        ),
        "permission" | "role" | "acl" => (
            vec!["auth", "authorize", "access", "grant", "deny", "allow"],
            vec!["forbid", "policy", "guard", "middleware", "can", "cannot"],
        ),

        // ── Database ────────────────────────────────────────────────────
        "db" | "database" | "data" | "storage" | "persist" | "persistence" => (
            vec!["sql", "query", "insert", "update", "delete", "select",
                 "table", "column", "row", "record", "schema", "migration"],
            vec!["pool", "connection", "transaction", "commit", "rollback",
                 "index", "foreign", "key", "primary", "constraint",
                 "postgres", "mysql", "sqlite", "mongodb", "redis", "orm",
                 "entity", "model", "repository", "dao", "crud",
                 "seed", "upsert", "join", "where", "order", "group",
                 "having", "aggregate", "count", "sum", "avg", "min", "max"],
        ),
        "query" | "sql" => (
            vec!["select", "insert", "update", "delete", "where", "join",
                 "table", "database", "db"],
            vec!["orm", "execute", "fetch", "find", "filter", "search"],
        ),
        "migration" | "schema" => (
            vec!["database", "db", "table", "column", "alter", "create"],
            vec!["drop", "add", "remove", "change", "rollback", "seed"],
        ),

        // ── HTTP / web ──────────────────────────────────────────────────
        "http" | "web" | "request" | "response" => (
            vec!["get", "post", "put", "patch", "delete", "route",
                 "endpoint", "api", "rest", "middleware", "handler",
                 "controller", "server", "client", "json"],
            vec!["header", "body", "param", "query", "path",
                 "graphql", "websocket", "status", "code", "error",
                 "redirect", "cors", "cookie", "session", "auth",
                 "bearer", "content", "type", "accept",
                 "encode", "decode", "serialize", "deserialize",
                 "xml", "form", "multipart", "url", "uri", "fetch", "axios",
                 "head", "options"],
        ),
        "api" | "rest" | "endpoint" => (
            vec!["http", "route", "controller", "handler", "request",
                 "response", "get", "post", "put", "delete", "json"],
            vec!["patch", "swagger", "openapi", "graphql", "version",
                 "middleware", "auth", "rate", "limit", "throttle"],
        ),
        "route" | "routing" => (
            vec!["http", "endpoint", "path", "url", "method",
                 "get", "post", "put", "delete", "handler", "controller"],
            vec!["uri", "middleware", "router", "match", "param", "query"],
        ),

        // ── Testing ─────────────────────────────────────────────────────
        "test" | "testing" | "spec" => (
            vec!["assert", "expect", "mock", "stub", "spy", "fake",
                 "unit", "integration", "suite", "case"],
            vec!["setup", "teardown", "before", "after", "each", "all",
                 "describe", "it", "should", "given", "when", "then",
                 "coverage", "jest", "mocha", "jasmine", "pytest",
                 "junit", "rspec", "cypress", "selenium", "playwright",
                 "fixture", "factory", "seed", "faker", "chance", "e2e", "end"],
        ),
        "mock" | "stub" | "fake" => (
            vec!["test", "spy", "double", "dummy", "simulate"],
            vec!["inject", "dependency", "override", "patch"],
        ),

        // ── Configuration ───────────────────────────────────────────────
        "config" | "configuration" | "setting" | "env" | "environment" => (
            vec!["option", "param", "parameter", "flag", "variable",
                 "default", "override", "load", "read", "write"],
            vec!["parse", "yaml", "yml", "json", "toml", "ini", "xml", "env",
                 "dotenv", "secret", "key", "value", "const", "constant"],
        ),

        // ── Error handling ──────────────────────────────────────────────
        "error" | "exception" | "fail" | "failure" | "bug" => (
            vec!["catch", "throw", "raise", "panic", "crash", "handle",
                 "recover", "retry", "fallback", "timeout"],
            vec!["cancel", "abort", "log", "trace", "debug", "warn", "info",
                 "message", "description", "detail", "cause", "stack",
                 "traceback", "report", "issue", "fix", "resolve"],
        ),

        // ── Logging ─────────────────────────────────────────────────────
        "log" | "logging" | "logger" => (
            vec!["debug", "info", "warn", "error", "trace", "fatal",
                 "console", "stdout", "stderr", "file", "output"],
            vec!["format", "level", "filter", "context", "span",
                 "metric", "telemetry", "observability", "monitor"],
        ),

        // ── Caching ─────────────────────────────────────────────────────
        "cache" | "caching" => (
            vec!["redis", "memcached", "memory", "lru", "ttl", "expire",
                 "invalidate", "evict", "hit", "miss"],
            vec!["warm", "cold", "store", "get", "set", "delete", "clear",
                 "flush", "stale", "fresh", "refresh", "preload"],
        ),

        // ── Cryptography ────────────────────────────────────────────────
        "crypto" | "encrypt" | "decrypt" | "cipher" | "hash" => (
            vec!["aes", "rsa", "sha", "md5", "hmac", "salt", "key",
                 "public", "private", "sign", "verify", "signature"],
            vec!["certificate", "cert", "tls", "ssl", "secure",
                 "random", "uuid", "nonce", "iv", "block", "stream"],
        ),

        // No expansion for unknown terms.
        _ => (vec![], vec![]),
    }
}

/// Edge cost for traversal based on relationship type.
fn edge_cost(rel_type: RelationshipType, config: &EvidenceConfig) -> f64 {
    match rel_type {
        RelationshipType::Calls => config.calls_cost,
        RelationshipType::Inherits | RelationshipType::Extends | RelationshipType::Implements => config.inherits_cost,
        RelationshipType::Imports | RelationshipType::Uses => config.imports_cost,
        RelationshipType::Accesses => config.accesses_cost,
        RelationshipType::Contains | RelationshipType::Defines => config.contains_cost,
        _ => 2.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphEdge, GraphNode, RelationshipType};
    use rustc_hash::FxHashMap;

    fn test_node(id: &str, name: &str, file: &str) -> GraphNode {
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), name.to_string());
        props.insert("file_path".to_string(), file.to_string());
        GraphNode { id: id.to_string(), label: "Function".to_string(), properties: props }
    }

    fn test_edge(src: &str, dst: &str, rel: RelationshipType) -> GraphEdge {
        GraphEdge {
            id: format!("{}->{}[{:?}]", src, dst, rel),
            source_id: src.to_string(),
            target_id: dst.to_string(),
            rel_type: rel,
            confidence: 1.0,
            reason: "test".to_string(),
            step: None,
        }
    }

    #[test]
    fn test_edge_cost() {
        let config = EvidenceConfig::default();
        assert_eq!(edge_cost(RelationshipType::Calls, &config), 1.0);
        assert_eq!(edge_cost(RelationshipType::Inherits, &config), 1.5);
        assert_eq!(edge_cost(RelationshipType::Imports, &config), 2.0);
        assert_eq!(edge_cost(RelationshipType::Accesses, &config), 1.8);
        assert_eq!(edge_cost(RelationshipType::Contains, &config), 3.0);
    }

    #[test]
    fn test_heuristic_perfect_match() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "login".to_string());
        props.insert("qualified_name".to_string(), "auth::login".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        let h = node_heuristic(&graph, &"sym:1".to_string(), 0.0, &["login".to_string()], &"sym:1".to_string());
        assert!((h - 0.0).abs() < 0.001, "Perfect name match should give h=0, got {}", h);
    }

    #[test]
    fn test_heuristic_no_match() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "foobar".to_string());
        props.insert("qualified_name".to_string(), "util::foobar".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        let h = node_heuristic(&graph, &"sym:1".to_string(), 0.0, &["auth".to_string()], &"sym:1".to_string());
        assert!((h - 1.0).abs() < 0.001, "No match should give h=1.0, got {}", h);
    }

    #[test]
    fn test_heuristic_partial_match() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "validate_token".to_string());
        props.insert("qualified_name".to_string(), "auth::validate_token".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // Name contains "token" but not "auth" — partial match.
        let h = node_heuristic(&graph, &"sym:1".to_string(), 0.0, &["auth".to_string()], &"sym:1".to_string());
        // qualified_name contains "auth" → text_rel = 0.4 → h = 0.6
        assert!((h - 0.6).abs() < 0.001, "Partial match (qualified name) should give h=0.6, got {}", h);
    }

    #[test]
    fn test_heuristic_with_fts_score() {
        let graph = KnowledgeGraph::new();
        // Seed node with FTS score 0.8: h = (1 - 0.8) * 1.0 = 0.2
        let h = node_heuristic(&graph, &"sym:99".to_string(), 0.8, &[], &"sym:99".to_string());
        assert!((h - 0.2).abs() < 0.001, "FTS score 0.8 should give h=0.2, got {}", h);
    }

    #[test]
    fn test_text_relevance_exact() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "login".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        let rel = text_relevance(&graph, &"sym:1".to_string(), &["login".to_string()]);
        assert!((rel - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_text_relevance_qualified() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "validate".to_string());
        props.insert("qualified_name".to_string(), "auth::validate".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        let rel = text_relevance(&graph, &"sym:1".to_string(), &["auth".to_string()]);
        assert!((rel - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_text_relevance_none() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "foobar".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        let rel = text_relevance(&graph, &"sym:1".to_string(), &["auth".to_string()]);
        assert!((rel - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_community_coherence_same() {
        let mut graph = KnowledgeGraph::new();
        // Community IDs are node IDs (strings), matching how from_store() populates them.
        graph.set_node_community("sym:1".to_string(), "com:auth".to_string());
        graph.set_node_community("sym:2".to_string(), "com:auth".to_string());

        let bonus = community_coherence(&graph, &"sym:1".to_string(), &"sym:2".to_string());
        assert!((bonus - 0.3).abs() < 0.001, "Same community should give 0.3, got {}", bonus);
    }

    #[test]
    fn test_community_coherence_different() {
        let mut graph = KnowledgeGraph::new();
        graph.set_node_community("sym:1".to_string(), "com:auth".to_string());
        graph.set_node_community("sym:2".to_string(), "com:db".to_string());

        let bonus = community_coherence(&graph, &"sym:1".to_string(), &"sym:2".to_string());
        assert!((bonus - 0.0).abs() < 0.001, "Different communities should give 0.0, got {}", bonus);
    }

    #[test]
    fn test_community_coherence_unassigned() {
        let graph = KnowledgeGraph::new();
        let bonus = community_coherence(&graph, &"sym:1".to_string(), &"sym:2".to_string());
        assert!((bonus - 0.0).abs() < 0.001, "Unassigned nodes should give 0.0, got {}", bonus);
    }

    #[test]
    fn test_heuristic_community_bonus() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "validate".to_string());
        props.insert("qualified_name".to_string(), "auth::validate".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // Both in the same community.
        graph.set_node_community("sym:1".to_string(), "com:auth".to_string());
        graph.set_node_community("sym:2".to_string(), "com:auth".to_string());

        // text_relevance for "auth" against "validate": qualified_name contains "auth" → 0.4
        // community_bonus: same community → 0.3
        // combined = 0.4 + 0.3 = 0.7
        // h = (1.0 - 0.7) * 1.0 = 0.3
        let h = node_heuristic(&graph, &"sym:1".to_string(), 0.0, &["auth".to_string()], &"sym:2".to_string());
        assert!((h - 0.3).abs() < 0.001, "Community bonus should reduce h to 0.3, got {}", h);
    }

    #[test]
    fn test_synonym_auth_matches_login() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "login".to_string());
        // No qualified_name containing "auth" — forces synonym path.
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // "auth" should match "login" via synonym expansion at score 0.5.
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["auth".to_string()]);
        assert!((rel - 0.5).abs() < 0.001, "auth->login synonym should give 0.5, got {}", rel);
    }

    #[test]
    fn test_synonym_auth_matches_verify_credentials() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "verify_credentials".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // "auth" → "verify" is a secondary synonym → 0.35.
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["auth".to_string()]);
        assert!((rel - 0.35).abs() < 0.001, "auth->verify secondary synonym should give 0.35, got {}", rel);
    }

    #[test]
    fn test_synonym_db_matches_repository() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "UserRepository".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Class".to_string(), properties: props });

        // "db" → "repository" is a secondary synonym → 0.35.
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["db".to_string()]);
        assert!((rel - 0.35).abs() < 0.001, "db->repository secondary synonym should give 0.35, got {}", rel);
    }

    #[test]
    fn test_synonym_http_matches_handler() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "request_handler".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // "http" should match "handler" via synonym expansion at 0.5.
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["http".to_string()]);
        assert!((rel - 0.5).abs() < 0.001, "http->handler synonym should give 0.5, got {}", rel);
    }

    #[test]
    fn test_synonym_no_match_for_unrelated() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "calculate_tax".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // "auth" should NOT match "calculate_tax".
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["auth".to_string()]);
        assert!((rel - 0.0).abs() < 0.001, "auth should not match calculate_tax, got {}", rel);
    }

    #[test]
    fn test_synonym_direct_match_beats_synonym() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "auth".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // Direct name match (1.0) should beat synonym match (0.5).
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["auth".to_string()]);
        assert!((rel - 1.0).abs() < 0.001, "Direct match should give 1.0, got {}", rel);
    }

    #[test]
    fn test_synonym_qualified_name_match() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "MyClass".to_string());
        props.insert("qualified_name".to_string(), "db::repository::MyClass".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Class".to_string(), properties: props });

        // "database" → "db" is a primary synonym, qualified name match → 0.25.
        let rel = text_relevance(&graph, &"sym:1".to_string(), &["database".to_string()]);
        assert!((rel - 0.25).abs() < 0.001, "database->db qualified match should give 0.25, got {}", rel);
    }

    #[test]
    fn test_word_boundary_rejects_substring() {
        // "batch" should NOT match "auth" — the "auth" in "batch" is not at a word boundary.
        assert!(!word_boundary_match("batch", "auth"));
        assert!(!word_boundary_match("reauthorize", "auth")); // "auth" mid-word
    }

    #[test]
    fn test_word_boundary_accepts_underscore() {
        assert!(word_boundary_match("auth_login", "auth"));
        assert!(word_boundary_match("auth_login", "login"));
        assert!(word_boundary_match("my_auth", "auth"));
    }

    #[test]
    fn test_word_boundary_accepts_camelcase() {
        assert!(word_boundary_match("UserRepository", "Repository"));
        assert!(word_boundary_match("UserRepository", "User"));
        assert!(!word_boundary_match("UserRepository", "er")); // not a boundary
    }

    #[test]
    fn test_word_boundary_accepts_module_separator() {
        assert!(word_boundary_match("auth::login", "auth"));
        assert!(word_boundary_match("auth::login", "login"));
        assert!(word_boundary_match("db::repository::MyClass", "db"));
        assert!(word_boundary_match("db::repository::MyClass", "repository"));
    }

    #[test]
    fn test_word_boundary_case_insensitive() {
        assert!(word_boundary_match("AuthLogin", "auth"));
        assert!(word_boundary_match("AUTH_LOGIN", "auth"));
        assert!(word_boundary_match("user_repository", "User"));
    }

    #[test]
    fn test_process_coherence_same() {
        let mut graph = KnowledgeGraph::new();
        graph.set_node_process("sym:1".to_string(), "proc:main_flow".to_string());
        graph.set_node_process("sym:2".to_string(), "proc:main_flow".to_string());

        let bonus = process_coherence(&graph, &"sym:1".to_string(), &"sym:2".to_string());
        assert!((bonus - 0.4).abs() < 0.001, "Same process should give 0.4, got {}", bonus);
    }

    #[test]
    fn test_process_coherence_different() {
        let mut graph = KnowledgeGraph::new();
        graph.set_node_process("sym:1".to_string(), "proc:flow_a".to_string());
        graph.set_node_process("sym:2".to_string(), "proc:flow_b".to_string());

        let bonus = process_coherence(&graph, &"sym:1".to_string(), &"sym:2".to_string());
        assert!((bonus - 0.0).abs() < 0.001, "Different processes should give 0.0, got {}", bonus);
    }

    #[test]
    fn test_heuristic_process_bonus() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "parse".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // Both in the same process.
        graph.set_node_process("sym:1".to_string(), "proc:main".to_string());
        graph.set_node_process("sym:2".to_string(), "proc:main".to_string());

        // text_relevance for "http" against "parse": no direct match, no synonym → 0.0
        // process_bonus: same process → 0.4
        // community_bonus: none → 0.0
        // combined = 0.0 + 0.4 + 0.0 = 0.4
        // h = (1.0 - 0.4) * 1.0 = 0.6
        let h = node_heuristic(&graph, &"sym:1".to_string(), 0.0, &["http".to_string()], &"sym:2".to_string());
        assert!((h - 0.6).abs() < 0.001, "Process bonus should give h=0.6, got {}", h);
    }

    #[test]
    fn test_heuristic_combined_bonuses() {
        let mut graph = KnowledgeGraph::new();
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "validate".to_string());
        props.insert("qualified_name".to_string(), "auth::validate".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        // Same community AND same process.
        graph.set_node_community("sym:1".to_string(), "com:auth".to_string());
        graph.set_node_community("sym:2".to_string(), "com:auth".to_string());
        graph.set_node_process("sym:1".to_string(), "proc:main".to_string());
        graph.set_node_process("sym:2".to_string(), "proc:main".to_string());

        // text_relevance for "auth" against "validate": qualified_name contains "auth" at boundary → 0.4
        // process_bonus: same process → 0.4
        // community_bonus: same community → 0.3
        // combined = 0.4 + 0.4 + 0.3 = 1.1 → capped at 1.0
        // h = (1.0 - 1.0) * 1.0 = 0.0
        let h = node_heuristic(&graph, &"sym:1".to_string(), 0.0, &["auth".to_string()], &"sym:2".to_string());
        assert!((h - 0.0).abs() < 0.001, "Combined bonuses should cap at h=0.0, got {}", h);
    }

    #[test]
    fn test_evidence_path_structure() {
        let path = EvidencePath {
            steps: vec![
                Evidence {
                    node_id: "n1".to_string(),
                    label: "main".to_string(),
                    file_path: "src/main.rs".to_string(),
                    line: 10,
                    relevance: 0.9,
                    via: EvidenceVia::TextMatch,
                },
                Evidence {
                    node_id: "n2".to_string(),
                    label: "handle_request".to_string(),
                    file_path: "src/handler.rs".to_string(),
                    line: 20,
                    relevance: 0.8,
                    via: EvidenceVia::CallChain,
                },
            ],
            confidence: 0.72,
            cost: 1.0,
            explanation: "Found via 1 hop, cost 1.00".to_string(),
            directions: vec![true],
            quality: PathQuality::default(),
        };
        assert_eq!(path.steps.len(), 2);
        assert_eq!(path.confidence, 0.72);
    }

    #[test]
    fn test_find_path_between_direct_call() {
        let mut graph = KnowledgeGraph::new();
        // A → B (A calls B). We want to find a path from A (id=1) to B (id=2).
        for (id, name) in [("sym:1", "foo"), ("sym:2", "bar")] {
            let mut props = FxHashMap::default();
            props.insert("name".to_string(), name.to_string());
            props.insert("file_path".to_string(), "src/lib.rs".to_string());
            props.insert("line".to_string(), "1".to_string());
            graph.add_node(GraphNode { id: id.to_string(), label: "Function".to_string(), properties: props });
        }
        graph.add_edge(test_edge("sym:1", "sym:2", RelationshipType::Calls));

        let store = crate::store::GraphStore::open_in_memory().unwrap();
        let config = EvidenceConfig { max_depth: 3, beam_width: 3, max_evidence: 5, ..Default::default() };
        let paths = find_path_between(&store, &graph, 1, 2, &config);
        // find_path_between always returns at least the seed as a single-step path.
        // If it finds the target, the best path has 2 steps (seed + target).
        assert!(!paths.is_empty(), "Should return at least the seed path");
        // The seed path has 1 step; a successful 2-step path may also be present.
        let best = &paths[0];
        assert!(best.confidence > 0.0, "Best path should have nonzero confidence");
        // Check that we found the target (bar) in at least one path
        let found_target = paths.iter().any(|p| p.steps.iter().any(|s| s.label == "bar"));
        assert!(found_target, "Should find a path that reaches the target 'bar'");
    }

    #[test]
    fn test_find_path_between_no_path() {
        let mut graph = KnowledgeGraph::new();
        // A and B are disconnected — only the seed path is returned
        for (id, name) in [("sym:1", "foo"), ("sym:2", "bar")] {
            let mut props = FxHashMap::default();
            props.insert("name".to_string(), name.to_string());
            props.insert("file_path".to_string(), "src/lib.rs".to_string());
            props.insert("line".to_string(), "1".to_string());
            graph.add_node(GraphNode { id: id.to_string(), label: "Function".to_string(), properties: props });
        }

        let store = crate::store::GraphStore::open_in_memory().unwrap();
        let config = EvidenceConfig { max_depth: 3, beam_width: 3, max_evidence: 5, ..Default::default() };
        let paths = find_path_between(&store, &graph, 1, 2, &config);
        // The seed is always returned as a single-step path even with no connection
        assert!(!paths.is_empty(), "Seed path is always returned");
        // But no path should reach the target
        let found_target = paths.iter().any(|p| p.steps.iter().any(|s| s.label == "bar"));
        assert!(!found_target, "Should NOT find a path to the disconnected target");
    }

    #[test]
    fn test_find_path_between_missing_node() {
        let graph = KnowledgeGraph::new();
        // Both nodes missing — should return empty
        let store = crate::store::GraphStore::open_in_memory().unwrap();
        let config = EvidenceConfig { max_depth: 3, beam_width: 3, max_evidence: 5, ..Default::default() };
        let paths = find_path_between(&store, &graph, 1, 2, &config);
        assert!(paths.is_empty(), "Should return empty when nodes don't exist in graph");
    }

    #[test]
    fn test_astar_beam_with_method_overrides() {
        let mut graph = KnowledgeGraph::new();
        // ParentClass::method → ChildClass::method (override)
        for (id, name) in [("sym:1", "ParentClass"), ("sym:2", "ChildClass"), ("sym:3", "method")] {
            let mut props = FxHashMap::default();
            props.insert("name".to_string(), name.to_string());
            props.insert("file_path".to_string(), "src/lib.rs".to_string());
            props.insert("line".to_string(), "1".to_string());
            graph.add_node(GraphNode { id: id.to_string(), label: "Class".to_string(), properties: props });
        }
        graph.add_edge(test_edge("sym:2", "sym:1", RelationshipType::MethodOverrides));

        let seed = SearchHit {
            node_id: 2, name: "ChildClass".into(), kind: "Class".into(),
            file_path: "src/lib.rs".into(), line: 1, score: 1.0, matched_text: String::new(),
        };
        let config = EvidenceConfig { max_depth: 3, beam_width: 3, max_evidence: 5, ..Default::default() };
        let paths = astar_beam(&graph, &seed, &config, &["child".to_string()]);
        // Should find a path to ParentClass via MethodOverrides
        let found_parent = paths.iter().any(|p| p.steps.iter().any(|s| s.label == "ParentClass"));
        assert!(found_parent, "A* should traverse MethodOverrides edges");
    }

    #[test]
    fn test_build_path_explanation_with_both_process_and_community() {
        let quality = PathQuality {
            process_steps: 2,
            community_steps: 3,
            forward_steps: 5,
            backward_steps: 0,
            process_id: Some("proc:main".to_string()),
        };
        let explanation = build_path_explanation(&quality, 5, 3.0);
        assert!(explanation.contains("follows execution flow"), "Should show process info: {}", explanation);
        assert!(explanation.contains("same community"), "Should show community info: {}", explanation);
        assert!(explanation.contains("5 hops"), "Should show hop count: {}", explanation);
    }
}
