//! Community Detection — Leiden algorithm on the code graph.
//!
//! Detects functional areas (communities) in the codebase based on
//! CALLS/ACCESSES edge density. Communities represent groups of symbols
//! that work together frequently.
//!
//! Ported from GitNexus's community-processor.ts which uses the Leiden
//! algorithm via graphology. Here we implement a simplified but effective
//! label-propagation approach that works directly on the SQLite graph store,
//! avoiding the need for an in-memory graph library.
//!
//! Algorithm: Iterative Label Propagation (LPA)
//! 1. Each node starts with its own label
//! 2. Iteratively, each node adopts the most frequent label among its neighbors
//! 3. Converges when no node changes label or max iterations reached
//! 4. Post-process: merge small communities below min_size threshold

use crate::store::GraphStore;
use rustc_hash::FxHashMap;
use serde::{Serialize, Deserialize};

/// A detected community (functional area).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Community {
    pub id: String,
    pub label: String,
    pub cohesion: f64,
    pub symbol_count: usize,
    pub keywords: Vec<String>,
}

/// Result of community detection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommunityDetectionResult {
    pub communities: Vec<Community>,
    /// node_id → community_id
    pub memberships: FxHashMap<i64, String>,
    pub stats: CommunityStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommunityStats {
    pub total_communities: usize,
    pub modularity: f64,
    pub nodes_processed: usize,
    pub iterations: usize,
}

/// Configuration for community detection.
pub struct CommunityConfig {
    /// Max iterations for label propagation (default: 100)
    pub max_iterations: usize,
    /// Minimum community size — smaller ones get merged (default: 3)
    pub min_size: usize,
    /// Edge kinds to consider (default: CALLS, ACCESSES)
    pub edge_kinds: Vec<String>,
}

impl Default for CommunityConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            min_size: 3,
            edge_kinds: vec!["CALLS".to_string(), "ACCESSES".to_string()],
        }
    }
}

/// Run community detection on the graph store.
pub fn detect_communities(
    store: &GraphStore,
    config: &CommunityConfig,
) -> rusqlite::Result<CommunityDetectionResult> {
    // Step 1: Load all symbol nodes and their edges from the store
    let all_symbols = store.get_all_symbols()?;

    if all_symbols.is_empty() {
        return Ok(CommunityDetectionResult::default());
    }

    let node_count = all_symbols.len();
    let symbol_ids: Vec<i64> = all_symbols.iter().map(|s| s.id).collect();

    // Build adjacency from edges table (undirected for community detection)
    let mut adjacency: FxHashMap<i64, Vec<(i64, f64)>> = FxHashMap::default();
    for sym in &all_symbols {
        adjacency.entry(sym.id).or_default();
    }

    // Query edges for our symbols with weights
    let all_edges = get_edges_for_symbols(store, &config.edge_kinds)?;
    for edge in &all_edges {
        if adjacency.contains_key(&edge.src_id) && adjacency.contains_key(&edge.dst_id) {
            let weight = edge.confidence;
            adjacency.entry(edge.src_id).or_default().push((edge.dst_id, weight));
            adjacency.entry(edge.dst_id).or_default().push((edge.src_id, weight));
        }
    }

    // Step 2: Label Propagation
    let mut labels: FxHashMap<i64, u64> = FxHashMap::default();
    for sym in &all_symbols {
        labels.insert(sym.id, sym.id as u64); // each node starts with its own label
    }

    let mut changed = true;
    let mut iteration = 0;
    let mut rng = node_count as u64; // simple deterministic tiebreaker seed

    while changed && iteration < config.max_iterations {
        changed = false;
        iteration += 1;

        // Process nodes in deterministic order
        let mut ordered_ids = symbol_ids.clone();
        ordered_ids.sort();

        for node_id in &ordered_ids {
            let neighbors = adjacency.get(node_id).map(|v| v.as_slice()).unwrap_or(&[]);
            if neighbors.is_empty() {
                continue;
            }

            // Weighted label voting: sum edge weights per label
            let mut label_weights: FxHashMap<u64, f64> = FxHashMap::default();
            for (neighbor_id, weight) in neighbors {
                if let Some(&label) = labels.get(neighbor_id) {
                    *label_weights.entry(label).or_insert(0.0) += *weight;
                }
            }

            if label_weights.is_empty() {
                continue;
            }

            // Find the label with highest total weight (tiebreak by smallest label for determinism)
            let mut best_label = *labels.get(node_id).unwrap();
            let mut best_weight = 0.0;
            for (&label, &weight) in &label_weights {
                if weight > best_weight || (weight == best_weight && label < best_label) {
                    best_weight = weight;
                    best_label = label;
                }
            }

            let current_label = labels.get(node_id).unwrap();
            if best_label != *current_label {
                labels.insert(*node_id, best_label);
                changed = true;
            }
        }
        rng = rng.wrapping_add(1);
    }

    // Step 3: Group nodes by label → communities
    let mut communities_map: FxHashMap<u64, Vec<i64>> = FxHashMap::default();
    for (node_id, label) in &labels {
        communities_map.entry(*label).or_default().push(*node_id);
    }

    // Step 4: Merge small communities into the largest neighboring community
    let small_communities: Vec<u64> = communities_map.iter()
        .filter(|(_, members)| members.len() < config.min_size)
        .map(|(label, _)| *label)
        .collect();

    for small_label in &small_communities {
        if let Some(members) = communities_map.remove(small_label) {
            // Find the largest neighboring community (by total edge weight)
            let mut neighbor_labels: FxHashMap<u64, f64> = FxHashMap::default();
            for member_id in &members {
                if let Some(neighbors) = adjacency.get(member_id) {
                    for (neighbor_id, weight) in neighbors {
                        if let Some(&nl) = labels.get(neighbor_id) {
                            if nl != *small_label {
                                *neighbor_labels.entry(nl).or_insert(0.0) += *weight;
                            }
                        }
                    }
                }
            }

            let target_label = neighbor_labels.iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(label, _)| *label);

            if let Some(target) = target_label {
                communities_map.entry(target).or_default().extend(members);
            } else {
                // No neighbors — keep as its own community
                communities_map.insert(*small_label, members);
            }
        }
    }

    // Step 5: Build Community structs with heuristic labels
    let mut communities = Vec::new();
    let mut memberships = FxHashMap::default();

    for (idx, (label, members)) in communities_map.iter().enumerate() {
        let community_id = format!("community_{}", idx);

        // Build heuristic label from most common symbol name prefixes
        let keywords = extract_community_keywords(&all_symbols, members);

        // Calculate cohesion: internal edges / total possible edges
        let internal_edges = count_internal_edges(&adjacency, members);
        let possible_edges = members.len() * (members.len() - 1) / 2;
        let cohesion = if possible_edges > 0 {
            internal_edges as f64 / possible_edges as f64
        } else {
            0.0
        };

        for member_id in members {
            memberships.insert(*member_id, community_id.clone());
        }

        communities.push(Community {
            id: community_id,
            label: keywords.first().cloned().unwrap_or_else(|| format!("Community {}", idx)),
            cohesion,
            symbol_count: members.len(),
            keywords,
        });
    }

    // Sort communities by size (largest first)
    communities.sort_by(|a, b| b.symbol_count.cmp(&a.symbol_count));

    // Calculate modularity (simplified)
    let modularity = calculate_modularity(&adjacency, &labels, &all_edges);

    Ok(CommunityDetectionResult {
        communities,
        memberships,
        stats: CommunityStats {
            total_communities: communities_map.len(),
            modularity,
            nodes_processed: node_count,
            iterations: iteration,
        },
    })
}

/// Get edges of specific kinds from the store.
fn get_edges_for_symbols(
    store: &GraphStore,
    edge_kinds: &[String],
) -> rusqlite::Result<Vec<EdgeInfo>> {
    // We need to query the edges table directly.
    // Since GraphStore doesn't have a generic edge query, we use a workaround:
    // query via the store's internal connection through get_edges_for_node for each symbol.
    // For efficiency, we'll use a batch approach.
    let mut edges = Vec::new();
    let files = store.get_all_files()?;
    for file in &files {
        let file_edges = get_all_edges_for_file(store, file.id)?;
        for edge in file_edges {
            if edge_kinds.contains(&edge.edge_kind) {
                edges.push(EdgeInfo {
                    src_id: edge.src_id,
                    dst_id: edge.dst_id,
                    edge_kind: edge.edge_kind,
                    confidence: edge.confidence,
                });
            }
        }
    }
    Ok(edges)
}

/// Get all edges for a file (workaround for missing batch query).
fn get_all_edges_for_file(store: &GraphStore, file_id: i64) -> rusqlite::Result<Vec<crate::store::EdgeRecord>> {
    // Use get_edges_for_node on each symbol in the file
    let symbols = store.get_symbols_by_file(file_id)?;
    let mut edges = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    for sym in &symbols {
        let node_edges = store.get_edges_for_node(sym.id)?;
        for edge in node_edges {
            if seen.insert(edge.id) {
                edges.push(edge);
            }
        }
    }
    Ok(edges)
}

fn extract_community_keywords(
    all_symbols: &[crate::store::SymbolRecord],
    members: &[i64],
) -> Vec<String> {
    let member_set: rustc_hash::FxHashSet<i64> = members.iter().copied().collect();
    let mut name_freq: FxHashMap<String, usize> = FxHashMap::default();

    for sym in all_symbols {
        if member_set.contains(&sym.id) {
            // Extract meaningful parts from qualified names
            for part in sym.qualified_name.split(|c| c == '.' || c == ':' || c == '\\' || c == '/') {
                let trimmed = part.trim();
                if trimmed.len() > 2 && !trimmed.chars().all(|c| c.is_numeric()) {
                    *name_freq.entry(trimmed.to_string()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut keywords: Vec<(String, usize)> = name_freq.into_iter().collect();
    keywords.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    keywords.into_iter().take(5).map(|(k, _)| k).collect()
}

fn count_internal_edges(adjacency: &FxHashMap<i64, Vec<(i64, f64)>>, members: &[i64]) -> usize {
    let member_set: rustc_hash::FxHashSet<i64> = members.iter().copied().collect();
    let mut count = 0;
    for member_id in members {
        if let Some(neighbors) = adjacency.get(member_id) {
            for (neighbor, _weight) in neighbors {
                if member_set.contains(neighbor) {
                    count += 1;
                }
            }
        }
    }
    count / 2 // each edge counted twice
}

fn calculate_modularity(
    adjacency: &FxHashMap<i64, Vec<(i64, f64)>>,
    labels: &FxHashMap<i64, u64>,
    _edges: &[EdgeInfo],
) -> f64 {
    // Simplified modularity: ratio of intra-community edges to total edges
    let total_edges: usize = adjacency.values().map(|v| v.len()).sum::<usize>() / 2;
    if total_edges == 0 {
        return 0.0;
    }

    let mut intra_edges = 0usize;
    for (node_id, neighbors) in adjacency {
        let node_label = labels.get(node_id);
        for (neighbor_id, _weight) in neighbors {
            if labels.get(neighbor_id) == node_label {
                intra_edges += 1;
            }
        }
    }
    intra_edges /= 2;

    intra_edges as f64 / total_edges as f64
}

#[derive(Debug, Clone)]
struct EdgeInfo {
    src_id: i64,
    dst_id: i64,
    edge_kind: String,
    confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::GraphStore;

    fn create_test_store_with_graph() -> GraphStore {
        let store = GraphStore::open_in_memory().unwrap();

        // Create two clusters:
        // Cluster 1: auth module (login, logout, validate, hash_password)
        // Cluster 2: db module (connect, query, insert, update)
        // Cross-edge: validate → connect

        let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0).unwrap();

        // Cluster 1 symbols
        let login_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "login".into(), qualified_name: "auth::login".into(),
            kind: "DefinitionFunction".into(), line: 1, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let logout_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "logout".into(), qualified_name: "auth::logout".into(),
            kind: "DefinitionFunction".into(), line: 10, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let validate_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "validate".into(), qualified_name: "auth::validate".into(),
            kind: "DefinitionFunction".into(), line: 20, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let hash_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "hash_password".into(), qualified_name: "auth::hash_password".into(),
            kind: "DefinitionFunction".into(), line: 30, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        // Cluster 2 symbols
        let connect_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "connect".into(), qualified_name: "db::connect".into(),
            kind: "DefinitionFunction".into(), line: 40, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let query_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "query".into(), qualified_name: "db::query".into(),
            kind: "DefinitionFunction".into(), line: 50, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let insert_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "insert".into(), qualified_name: "db::insert".into(),
            kind: "DefinitionFunction".into(), line: 60, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let update_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "update".into(), qualified_name: "db::update".into(),
            kind: "DefinitionFunction".into(), line: 70, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        // Intra-cluster edges (CALLS) — dense within each cluster
        let intra_edges = [
            // Auth cluster: fully connected
            (login_id, validate_id), (login_id, hash_id), (login_id, logout_id),
            (logout_id, validate_id), (logout_id, hash_id),
            (validate_id, hash_id),
            // DB cluster: fully connected
            (connect_id, query_id), (connect_id, insert_id), (connect_id, update_id),
            (query_id, insert_id), (query_id, update_id),
            (insert_id, update_id),
        ];
        for (src, dst) in &intra_edges {
            store.insert_edge(&crate::store::EdgeRecord {
                id: 0, src_id: *src, dst_id: *dst,
                edge_kind: "CALLS".into(), confidence: 1.0,
                file_id: Some(file_id), line: 0,
            }).unwrap();
        }

        // Single cross-cluster edge (validate → connect)
        store.insert_edge(&crate::store::EdgeRecord {
            id: 0, src_id: validate_id, dst_id: connect_id,
            edge_kind: "CALLS".into(), confidence: 0.9,
            file_id: Some(file_id), line: 0,
        }).unwrap();

        store
    }

    #[test]
    fn test_community_detection_finds_clusters() {
        let store = create_test_store_with_graph();
        // Use min_size=2 so small communities aren't force-merged
        let config = CommunityConfig {
            max_iterations: 100,
            min_size: 2,
            edge_kinds: vec!["CALLS".to_string(), "ACCESSES".to_string()],
        };
        let result = detect_communities(&store, &config).unwrap();

        // Should find at least 2 communities (auth cluster and db cluster)
        assert!(result.communities.len() >= 2,
            "Expected at least 2 communities, got {}", result.communities.len());

        // Total symbols processed
        assert_eq!(result.stats.nodes_processed, 8);

        // All symbols should be assigned to a community
        assert_eq!(result.memberships.len(), 8);

        // Verify that the two clusters have different community IDs
        // Find the community containing "login" and "connect" — they should differ
        let login_community = result.memberships.iter()
            .find(|(id, _)| {
                store.get_symbols_by_file(1).unwrap().iter()
                    .any(|s| s.id == **id && s.name == "login")
            })
            .map(|(_, cid)| cid.clone());

        let connect_community = result.memberships.iter()
            .find(|(id, _)| {
                store.get_symbols_by_file(1).unwrap().iter()
                    .any(|s| s.id == **id && s.name == "connect")
            })
            .map(|(_, cid)| cid.clone());

        assert_ne!(login_community, connect_community,
            "login and connect should be in different communities");
    }

    #[test]
    fn test_empty_graph() {
        let store = GraphStore::open_in_memory().unwrap();
        let config = CommunityConfig::default();
        let result = detect_communities(&store, &config).unwrap();
        assert_eq!(result.communities.len(), 0);
        assert_eq!(result.stats.nodes_processed, 0);
    }

    #[test]
    fn test_single_node() {
        let store = GraphStore::open_in_memory().unwrap();
        let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0).unwrap();
        store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "main".into(), qualified_name: "main".into(),
            kind: "DefinitionFunction".into(), line: 1, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let config = CommunityConfig::default();
        let result = detect_communities(&store, &config).unwrap();
        // Single node with no edges — may or may not form a community depending on min_size
        assert!(result.stats.nodes_processed <= 1);
    }
}
