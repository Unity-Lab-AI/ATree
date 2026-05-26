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
            // LPA typically converges in 10-20 iterations on code graphs.
            // 100 was causing 49-minute runs on 400K-node graphs.
            max_iterations: 20,
            min_size: 3,
            edge_kinds: vec!["CALLS".to_string(), "ACCESSES".to_string()],
        }
    }
}

/// Run community detection on the graph store.
///
/// Uses label propagation with CSR-like adjacency for cache-friendly iteration.
/// For a 400K-node graph with 700K edges, this completes in ~5 seconds vs ~49
/// minutes with the old per-symbol SQL query + FxHashMap approach.
pub fn detect_communities(
    store: &GraphStore,
    config: &CommunityConfig,
) -> rusqlite::Result<CommunityDetectionResult> {
    // Step 1: Load all symbol IDs
    let all_symbols = store.get_all_symbols()?;

    if all_symbols.is_empty() {
        return Ok(CommunityDetectionResult::default());
    }

    let node_count = all_symbols.len();

    // Build a dense id_to_idx mapping so we can use Vec-based arrays instead of FxHashMap
    let symbol_ids: Vec<i64> = all_symbols.iter().map(|s| s.id).collect();
    let id_to_idx: FxHashMap<i64, usize> = symbol_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    // Step 2: Load edges in a single batch query and build CSR adjacency
    let all_edges = get_edges_for_symbols(store, &config.edge_kinds)?;

    // Build adjacency using Vec of Vecs (indexed by dense idx)
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); node_count];
    for edge in &all_edges {
        if let (Some(&si), Some(&di)) = (id_to_idx.get(&edge.src_id), id_to_idx.get(&edge.dst_id)) {
            let w = edge.confidence;
            adjacency[si].push((di, w));
            adjacency[di].push((si, w));
        }
    }

    // Step 3: Label Propagation — only process nodes with edges.
    // Isolated nodes (no edges) keep their own label and are handled in grouping.
    let mut labels: Vec<u64> = (0..node_count as u64).collect();
    let mut changed = true;
    let mut iteration = 0;

    // Build a list of node indices that actually have neighbors (skip isolated nodes).
    // On a typical code graph, 30-50% of symbols are isolated (no CALLS/ACCESSES edges).
    let active_nodes: Vec<usize> = (0..node_count)
        .filter(|&i| !adjacency[i].is_empty())
        .collect();

    while changed && iteration < config.max_iterations {
        changed = false;
        iteration += 1;

        for &node_idx in &active_nodes {
            let neighbors = &adjacency[node_idx];
            let current_label = labels[node_idx];

            // Weighted label voting using small linear scan (faster than FxHashMap
            // for typical code graph degree of 2-10).
            let mut scratch: Vec<(u64, f64)> = Vec::with_capacity(neighbors.len());
            for &(neighbor_idx, weight) in neighbors {
                let nlabel = labels[neighbor_idx];
                if let Some(entry) = scratch.iter_mut().find(|(l, _)| *l == nlabel) {
                    entry.1 += weight;
                } else {
                    scratch.push((nlabel, weight));
                }
            }

            if scratch.is_empty() {
                continue;
            }

            let mut best_label = current_label;
            let mut best_weight = 0.0;
            for &(label, weight) in &scratch {
                if weight > best_weight || (weight == best_weight && label < best_label) {
                    best_weight = weight;
                    best_label = label;
                }
            }

            if best_label != current_label {
                labels[node_idx] = best_label;
                changed = true;
            }
        }
    }

    // Step 4: Group nodes by label → communities
    let mut communities_map: FxHashMap<u64, Vec<i64>> = FxHashMap::default();
    for (idx, &label) in labels.iter().enumerate() {
        communities_map.entry(label).or_default().push(symbol_ids[idx]);
    }

    // Step 5: Merge small communities into the largest neighboring community
    let small_communities: Vec<u64> = communities_map.iter()
        .filter(|(_, members)| members.len() < config.min_size)
        .map(|(label, _)| *label)
        .collect();

    for small_label in &small_communities {
        if let Some(members) = communities_map.remove(small_label) {
            let mut neighbor_labels: FxHashMap<u64, f64> = FxHashMap::default();
            for member_id in &members {
                if let Some(&midx) = id_to_idx.get(member_id) {
                    for &(nbr_idx, weight) in &adjacency[midx] {
                        let nl = labels[nbr_idx];
                        if nl != *small_label {
                            *neighbor_labels.entry(nl).or_insert(0.0) += weight;
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
                communities_map.insert(*small_label, members);
            }
        }
    }

    // Step 5: Build Community structs with heuristic labels
    let mut communities = Vec::new();
    let mut memberships = FxHashMap::default();

    for (idx, (_label, members)) in communities_map.iter().enumerate() {
        let community_id = format!("community_{}", idx);

        // Build heuristic label from most common symbol name prefixes
        let keywords = extract_community_keywords(&all_symbols, members);

        // Calculate cohesion: internal edges / total possible edges
        let internal_edges = count_internal_edges(&adjacency, &id_to_idx, members);
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
    let modularity = calculate_modularity(&adjacency, &labels);

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

/// Persist community memberships as MEMBER_OF edges in the graph store.
pub fn store_memberships(
    store: &GraphStore,
    result: &CommunityDetectionResult,
) -> rusqlite::Result<usize> {
    let mut count = 0;
    for (symbol_id, community_id) in &result.memberships {
        if let Ok(Some(file_id)) = store.get_file_id_for_symbol(*symbol_id) {
            store.insert_edge(&crate::store::EdgeRecord {
                id: 0,
                src_id: *symbol_id,
                dst_id: community_node_id(community_id),
                edge_kind: "MEMBER_OF".to_string(),
                confidence: 1.0,
                file_id: Some(file_id),
                line: 0,
            })?;
            count += 1;
        }
    }
    Ok(count)
}

/// Convert a community ID string to a numeric node ID for edge storage.
/// community_0 → 1_000_000, community_1 → 1_000_001, etc.
fn community_node_id(community_id: &str) -> i64 {
    let n = community_id
        .strip_prefix("community_")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    1_000_000 + n
}

/// Get edges of specific kinds from the store using a single batched query.
///
/// Replaces the old O(symbols) per-node queries with one SQL query that
/// filters by edge_kind. For a 700K-edge graph this is the difference between
/// ~49 minutes and ~2 seconds.
fn get_edges_for_symbols(
    store: &GraphStore,
    edge_kinds: &[String],
) -> rusqlite::Result<Vec<EdgeInfo>> {
    if edge_kinds.is_empty() {
        return Ok(Vec::new());
    }

    // Build a single query with IN clause for edge kinds.
    let placeholders: Vec<String> = (0..edge_kinds.len()).map(|i| format!("?{}", i + 1)).collect();
    let query = format!(
        "SELECT src_id, dst_id, confidence FROM edges WHERE edge_kind IN ({})",
        placeholders.join(", ")
    );

    let mut stmt = store.conn().prepare(&query)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(edge_kinds.iter().map(|s| s.as_str())), |row| {
        Ok(EdgeInfo {
            src_id: row.get(0)?,
            dst_id: row.get(1)?,
            confidence: row.get(2)?,
        })
    })?;
    rows.collect()
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
            for part in sym.qualified_name.split(['.', ':', '\\', '/']) {
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

fn count_internal_edges(
    adjacency: &[Vec<(usize, f64)>],
    id_to_idx: &FxHashMap<i64, usize>,
    members: &[i64],
) -> usize {
    let member_set: rustc_hash::FxHashSet<usize> = members
        .iter()
        .filter_map(|id| id_to_idx.get(id).copied())
        .collect();
    let mut count = 0;
    for member_id in members {
        if let Some(&idx) = id_to_idx.get(member_id) {
            for &(neighbor_idx, _weight) in &adjacency[idx] {
                if member_set.contains(&neighbor_idx) {
                    count += 1;
                }
            }
        }
    }
    count / 2 // each edge counted twice
}

fn calculate_modularity(
    adjacency: &[Vec<(usize, f64)>],
    labels: &[u64],
) -> f64 {
    // Simplified modularity: ratio of intra-community edges to total edges
    let total_edges: usize = adjacency.iter().map(|v| v.len()).sum::<usize>() / 2;
    if total_edges == 0 {
        return 0.0;
    }

    let mut intra_edges = 0usize;
    for (node_idx, neighbors) in adjacency.iter().enumerate() {
        let node_label = labels[node_idx];
        for &(neighbor_idx, _weight) in neighbors {
            if labels[neighbor_idx] == node_label {
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

        let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0, None).unwrap();

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
        let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0, None).unwrap();
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
