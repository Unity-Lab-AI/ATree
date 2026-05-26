//! C3 Linearization — Python MRO algorithm.
//!
//! Ported directly from GitNexus's `resolve.ts` `c3Linearize()`.
//!
//! C3 linearization produces a deterministic method resolution order for
//! Python-style multiple inheritance. It handles diamond inheritance correctly
//! and detects cyclic/inconsistent hierarchies.
//!
//! Uses an iterative approach (explicit stack) instead of recursion to avoid
//! stack overflow on deep hierarchies (10K+ levels).

const ENTER: u8 = 0;
const MERGE: u8 = 1;

use std::collections::HashMap;

/// Compute C3 linearization for a class given a parent map.
/// Returns the linearized list of ancestor IDs (excluding the class itself),
/// or None if linearization fails (inconsistent or cyclic hierarchy).
///
/// `parent_map`: maps each class ID to its direct parent IDs (declaration order).
/// `cache`: memoization cache across multiple calls.
pub fn c3_linearize(
    class_id: &str,
    parent_map: &HashMap<String, Vec<String>>,
    cache: &mut HashMap<String, Option<Vec<String>>>,
) -> Option<Vec<String>> {
    if let Some(cached) = cache.get(class_id) {
        return cached.clone();
    }

    // Iterative C3 using explicit stack with ENTER/MERGE phases.
    // Each frame goes through:
    //   ENTER (0) – check cache / cycle, push parent frames first
    //   MERGE (1) – all parent linearizations cached, merge C3-style

    let mut stack: Vec<(String, u8)> = vec![(class_id.to_string(), ENTER)];
    let mut visiting: HashMap<String, bool> = HashMap::new();

    while let Some((id, phase)) = stack.last().cloned() {
        if phase == ENTER {
            // Check cache
            if cache.contains_key(&id) {
                stack.pop();
                continue;
            }

            // Cycle detection
            if visiting.get(&id).copied().unwrap_or(false) {
                cache.insert(id.clone(), None);
                stack.pop();
                continue;
            }
            visiting.insert(id.clone(), true);

            let direct_parents = parent_map.get(&id).cloned().unwrap_or_default();
            if direct_parents.is_empty() {
                visiting.insert(id.clone(), false);
                cache.insert(id.clone(), Some(Vec::new()));
                stack.pop();
                continue;
            }

            // Switch to MERGE, push parents that need computing
            let top = stack.last_mut().unwrap();
            top.1 = MERGE;

            let mut all_cached = true;
            for pid in direct_parents.iter().rev() {
                if !cache.contains_key(pid) {
                    stack.push((pid.clone(), ENTER));
                    all_cached = false;
                }
            }
            if !all_cached {
                continue;
            }
        }

        // MERGE phase
        stack.pop();
        let direct_parents = parent_map.get(&id).cloned().unwrap_or_default();

        // Build parent linearizations from cache
        let mut parent_linearizations: Vec<Vec<String>> = Vec::new();
        let mut failed = false;
        for pid in &direct_parents {
            match cache.get(pid) {
                Some(Some(plin)) => {
                    let mut v = vec![pid.clone()];
                    v.extend(plin.clone());
                    parent_linearizations.push(v);
                }
                Some(None) => {
                    failed = true;
                    break;
                }
                None => {
                    failed = true;
                    break;
                }
            }
        }

        if failed {
            visiting.insert(id.clone(), false);
            cache.insert(id, None);
            continue;
        }

        // Add direct parents list as final sequence
        let mut sequences = parent_linearizations;
        sequences.push(direct_parents.clone());

        let seq_count = sequences.len();
        let mut heads: Vec<usize> = vec![0; seq_count];
        let mut result: Vec<String> = Vec::new();

        // Tail-count map: how many sequences contain this id at index > head
        let mut tail_count: HashMap<String, usize> = HashMap::new();
        for seq in &sequences {
            for item in seq.iter().skip(1) {
                *tail_count.entry(item.clone()).or_insert(0) += 1;
            }
        }

        let mut remaining: usize = sequences.iter().map(|s| s.len()).sum();
        let mut inconsistent = false;

        while remaining > 0 {
            let mut found: Option<String> = None;
            for si in 0..seq_count {
                if heads[si] >= sequences[si].len() {
                    continue;
                }
                let candidate = &sequences[si][heads[si]];
                if tail_count.get(candidate).copied().unwrap_or(0) == 0 {
                    found = Some(candidate.clone());
                    break;
                }
            }

            match found {
                None => {
                    inconsistent = true;
                    break;
                }
                Some(head) => {
                    result.push(head.clone());
                    for si in 0..seq_count {
                        if heads[si] >= sequences[si].len() {
                            continue;
                        }
                        if sequences[si][heads[si]] == head {
                            heads[si] += 1;
                            remaining -= 1;
                            if heads[si] < sequences[si].len() {
                                let promoted = &sequences[si][heads[si]];
                                let count = tail_count.entry(promoted.clone()).or_insert(0);
                                if *count <= 1 {
                                    tail_count.remove(promoted);
                                } else {
                                    *count -= 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        visiting.insert(id.clone(), false);
        if inconsistent {
            cache.insert(id, None);
        } else {
            cache.insert(id.clone(), Some(result));
        }
    }

    cache.get(class_id).cloned().unwrap_or(None)
}

/// Gather all ancestor IDs in BFS / topological order.
/// Returns the linearized list of ancestor IDs (excluding the class itself).
pub fn gather_ancestors(
    class_id: &str,
    parent_map: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut visited: HashMap<String, bool> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut queue: Vec<String> = parent_map
        .get(class_id)
        .cloned()
        .unwrap_or_default();
    let mut head: usize = 0;

    while head < queue.len() {
        let id = queue[head].clone();
        head += 1;
        if visited.get(&id).copied().unwrap_or(false) {
            continue;
        }
        visited.insert(id.clone(), true);
        order.push(id.clone());
        if let Some(grandparents) = parent_map.get(&id) {
            for gp in grandparents {
                if !visited.get(gp).copied().unwrap_or(false) {
                    queue.push(gp.clone());
                }
            }
        }
    }

    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c3_simple_diamond() {
        // Classic diamond: D(B, C), B(A), C(A)
        // C3 MRO: D, B, C, A
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        parent_map.insert("B".to_string(), vec!["A".to_string()]);
        parent_map.insert("C".to_string(), vec!["A".to_string()]);
        parent_map.insert(
            "D".to_string(),
            vec!["B".to_string(), "C".to_string()],
        );

        let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let d_mro = c3_linearize("D", &parent_map, &mut cache).unwrap();
        assert_eq!(d_mro, vec!["B", "C", "A"]);
    }

    #[test]
    fn c3_linear_chain() {
        // A → B → C → D
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        parent_map.insert("B".to_string(), vec!["A".to_string()]);
        parent_map.insert("C".to_string(), vec!["B".to_string()]);
        parent_map.insert("D".to_string(), vec!["C".to_string()]);

        let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let d_mro = c3_linearize("D", &parent_map, &mut cache).unwrap();
        assert_eq!(d_mro, vec!["C", "B", "A"]);
    }

    #[test]
    fn c3_no_parents() {
        let parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let result = c3_linearize("A", &parent_map, &mut cache).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn c3_cycle_detection() {
        // A → B → C → A (cycle)
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        parent_map.insert("A".to_string(), vec!["B".to_string()]);
        parent_map.insert("B".to_string(), vec!["C".to_string()]);
        parent_map.insert("C".to_string(), vec!["A".to_string()]);

        let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let result = c3_linearize("A", &parent_map, &mut cache);
        assert!(result.is_none(), "Should detect cycle");
    }

    #[test]
    fn c3_complex_diamond() {
        //       A
        //      / \
        //     B   C
        //    / \ / \
        //   D   E   F
        //    \  |  /
        //       G
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        parent_map.insert("B".to_string(), vec!["A".to_string()]);
        parent_map.insert("C".to_string(), vec!["A".to_string()]);
        parent_map.insert("D".to_string(), vec!["B".to_string()]);
        parent_map.insert("E".to_string(), vec!["B".to_string(), "C".to_string()]);
        parent_map.insert("F".to_string(), vec!["C".to_string()]);
        parent_map.insert(
            "G".to_string(),
            vec!["D".to_string(), "E".to_string(), "F".to_string()],
        );

        let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
        let g_mro = c3_linearize("G", &parent_map, &mut cache).unwrap();
        // G's C3 MRO: G, D, E, B, F, C, A  # Verified against Python C3
        assert_eq!(g_mro, vec!["D", "E", "B", "F", "C", "A"]);
    }

    #[test]
    fn gather_ancestors_bfs() {
        let mut parent_map: HashMap<String, Vec<String>> = HashMap::new();
        parent_map.insert("B".to_string(), vec!["A".to_string()]);
        parent_map.insert("C".to_string(), vec!["A".to_string()]);
        parent_map.insert(
            "D".to_string(),
            vec!["B".to_string(), "C".to_string()],
        );

        let ancestors = gather_ancestors("D", &parent_map);
        // BFS from D: B, C, then A (from both B and C, deduped)
        assert!(ancestors.contains(&"A".to_string()));
        assert!(ancestors.contains(&"B".to_string()));
        assert!(ancestors.contains(&"C".to_string()));
        assert_eq!(ancestors.len(), 3);
    }
}
