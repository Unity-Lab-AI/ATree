//! Parallel force-directed graph layout engine.
//!
//! Computes 2D positions for knowledge-graph nodes using a Barnes-Hut
//! accelerated force-directed algorithm. Runs in parallel using the same
//! work-stealing approach as ATree's filesystem scanner.
//!
//! The layout is deterministic given the same seed and produces
//! community-colored, hierarchy-aware positions suitable for Canvas rendering.

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::thread;

// ── Public types ─────────────────────────────────────────────────────────────

/// A 2D point.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A laid-out node ready for rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub file_path: String,
    pub line: Option<i64>,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub color: String,
    pub community: Option<usize>,
    pub depth: i32,
}

/// A laid-out edge ready for rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutEdge {
    pub source: String,
    pub target: String,
    pub rel_type: String,
    pub confidence: f64,
}

/// Complete layout result — JSON-serializable for the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLayout {
    pub nodes: Vec<LayoutNode>,
    pub edges: Vec<LayoutEdge>,
    pub stats: LayoutStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub iterations: usize,
    pub elapsed_ms: u64,
}

/// Layout algorithm to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutAlgorithm {
    /// Force-directed (Barnes-Hut). Good for general graphs, community overview.
    ForceDirected,
    /// Layered DAG (Sugiyama-style). Good for call graphs, dependency flow.
    /// Nodes are arranged in horizontal layers from upstream (left) to downstream (right).
    LayeredDAG,
}

/// Layout configuration.
#[derive(Debug, Clone)]
pub struct LayoutConfig {
    /// Which layout algorithm to use.
    pub algorithm: LayoutAlgorithm,
    /// Number of force simulation iterations (for ForceDirected).
    pub iterations: usize,
    /// Repulsion strength (Coulomb's law).
    pub repulsion: f64,
    /// Spring length for connected nodes.
    pub spring_length: f64,
    /// Spring stiffness.
    pub spring_strength: f64,
    /// Gravity toward center (prevents drift).
    pub gravity: f64,
    /// Barnes-Hut theta (0 = exact, 1 = fast).
    pub theta: f64,
    /// Thread count (0 = auto = half cores).
    pub threads: usize,
    /// Random seed for deterministic layout.
    pub seed: u64,
    /// Edge types to include. Empty = all.
    pub edge_filter: Vec<String>,
    /// Node types to include. Empty = all.
    pub node_type_filter: Vec<String>,
    /// For LayeredDAG: horizontal spacing between layers.
    pub layer_spacing: f64,
    /// For LayeredDAG: vertical spacing within a layer.
    pub node_spacing: f64,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            algorithm: LayoutAlgorithm::ForceDirected,
            iterations: 300,
            repulsion: 5000.0,
            spring_length: 80.0,
            spring_strength: 0.05,
            gravity: 0.01,
            theta: 0.8,
            threads: 0,
            seed: 42,
            edge_filter: vec![],
            node_type_filter: vec![],
            layer_spacing: 200.0,
            node_spacing: 80.0,
        }
    }
}

// ── Internal types ───────────────────────────────────────────────────────────

/// Compact node representation for the simulation.
struct SimNode {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    mass: f64,
    #[allow(dead_code)]
    community: Option<usize>,
}

/// Edge used during simulation.
struct SimEdge {
    source: usize,
    target: usize,
    strength: f64,
}

/// Barnes-Hut quadtree node.
enum QuadTree {
    Empty,
    Leaf {
        x: f64,
        y: f64,
        mass: f64,
        cx: f64,
        cy: f64,
    },
    Internal {
        mass: f64,
        cx: f64,
        cy: f64,
        children: [Box<QuadTree>; 4],
    },
}

// ── Deterministic RNG (xoshiro256++) ────────────────────────────────────────

struct FastRng {
    s: [u64; 4],
}

impl FastRng {
    fn new(seed: u64) -> Self {
        let mut s = [seed, seed.wrapping_add(0x9e3779b97f4a7c15), seed.wrapping_add(0xbf58476d1ce4e5b9), seed.wrapping_add(0x94d049bb133111eb)];
        for _ in 0..12 {
            Self::next(&mut s);
        }
        Self { s }
    }

    fn next(s: &mut [u64; 4]) -> u64 {
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    fn next_f64(&mut self) -> f64 {
        (Self::next(&mut self.s) >> 11) as f64 / (1u64 << 53) as f64
    }

    #[allow(dead_code)]
    fn next_range(&mut self, min: f64, max: f64) -> f64 {
        min + self.next_f64() * (max - min)
    }
}

// ── Quadtree ─────────────────────────────────────────────────────────────────

impl QuadTree {
    fn new() -> Self {
        QuadTree::Empty
    }

    fn insert(&mut self, px: f64, py: f64, new_mass: f64, min_x: f64, min_y: f64, size: f64) {
        if size < 0.01 {
            // Prevent infinite recursion for coincident points
            match self {
                QuadTree::Empty => {
                    *self = QuadTree::Leaf { x: px, y: py, mass: new_mass, cx: px, cy: py };
                }
                QuadTree::Leaf { x: _, y: _, mass: m, cx, cy } => {
                    let total = *m + new_mass;
                    *cx = (*cx * *m + px * new_mass) / total;
                    *cy = (*cy * *m + py * new_mass) / total;
                    *m = total;
                }
                _ => {}
            }
            return;
        }

        match self {
            QuadTree::Empty => {
                *self = QuadTree::Leaf { x: px, y: py, mass: new_mass, cx: px, cy: py };
            }
            QuadTree::Leaf { x: _x, y: _y, mass: m, cx, cy } => {
                let old_mass = *m;
                let old_x = *cx;
                let old_y = *cy;
                *m += new_mass;
                *cx = (old_x * old_mass + px * new_mass) / *m;
                *cy = (old_y * old_mass + py * new_mass) / *m;

                // Subdivide
                let children: [Box<QuadTree>; 4] = [
                    Box::new(QuadTree::Empty),
                    Box::new(QuadTree::Empty),
                    Box::new(QuadTree::Empty),
                    Box::new(QuadTree::Empty),
                ];
                let mut new_self = QuadTree::Internal {
                    mass: *m,
                    cx: *cx,
                    cy: *cy,
                    children,
                };

                // Re-insert old point
                let (qx, qy, qs) = quadrant(old_x, old_y, min_x, min_y, size);
                new_self.insert(old_x, old_y, old_mass, qx, qy, qs);

                // Insert new point
                let (qx, qy, qs) = quadrant(px, py, min_x, min_y, size);
                new_self.insert(px, py, new_mass, qx, qy, qs);

                *self = new_self;
            }
            QuadTree::Internal { mass, cx, cy, children } => {
                let old_mass = *mass;
                let total = old_mass + new_mass;
                *mass = total;
                *cx = (*cx * old_mass + px * new_mass) / total;
                *cy = (*cy * old_mass + py * new_mass) / total;

                let (qx, qy, qs) = quadrant(px, py, min_x, min_y, size);
                children[quadrant_idx(px, py, min_x, min_y, size)]
                    .insert(px, py, new_mass, qx, qy, qs);
            }
        }
    }

    fn compute_force(&self, px: f64, py: f64, theta: f64, repulsion: f64) -> (f64, f64) {
        match self {
            QuadTree::Empty => (0.0, 0.0),
            QuadTree::Leaf { cx, cy, mass, .. } => {
                let dx = px - *cx;
                let dy = py - *cy;
                let dist_sq = dx * dx + dy * dy + 1.0;
                let dist = dist_sq.sqrt();
                let force = repulsion * *mass / dist_sq;
                (-force * dx / dist, -force * dy / dist)
            }
            QuadTree::Internal { mass, cx, cy, children } => {
                let dx = px - *cx;
                let dy = py - *cy;
                let dist_sq = dx * dx + dy * dy + 1.0;
                let dist = dist_sq.sqrt();
                let size_estimate = dist; // approximate
                if size_estimate / dist < theta {
                    let force = repulsion * *mass / dist_sq;
                    (-force * dx / dist, -force * dy / dist)
                } else {
                    let mut fx = 0.0;
                    let mut fy = 0.0;
                    for child in children.iter() {
                        let (cfx, cfy) = child.compute_force(px, py, theta, repulsion);
                        fx += cfx;
                        fy += cfy;
                    }
                    (fx, fy)
                }
            }
        }
    }
}

fn quadrant(px: f64, py: f64, min_x: f64, min_y: f64, size: f64) -> (f64, f64, f64) {
    let half = size / 2.0;
    let idx = quadrant_idx(px, py, min_x, min_y, size);
    let (ox, oy) = match idx {
        0 => (min_x, min_y),
        1 => (min_x + half, min_y),
        2 => (min_x, min_y + half),
        _ => (min_x + half, min_y + half),
    };
    (ox, oy, half)
}

fn quadrant_idx(px: f64, py: f64, min_x: f64, min_y: f64, size: f64) -> usize {
    let half = size / 2.0;
    let mx = min_x + half;
    let my = min_y + half;
    ((if px >= mx { 1 } else { 0 }) + (if py >= my { 2 } else { 0 })) as usize
}

// ── Color palette ────────────────────────────────────────────────────────────

const COMMUNITY_COLORS: &[&str] = &[
    "#6366f1", "#8b5cf6", "#a855f7", "#d946ef", "#ec4899",
    "#f43f5e", "#ef4444", "#f97316", "#eab308", "#84cc16",
    "#22c55e", "#14b8a6", "#06b6d4", "#0ea5e9", "#3b82f6",
    "#6b7280", "#78716c", "#71717a", "#737373", "#7c6f64",
];

fn community_color(id: Option<usize>) -> String {
    match id {
        Some(id) => COMMUNITY_COLORS[id % COMMUNITY_COLORS.len()].to_string(),
        None => "#6b7280".to_string(),
    }
}

fn node_color(node_type: &str, community: Option<usize>) -> String {
    match node_type {
        "Function" | "Method" => "#06b6d4".to_string(),
        "Class" | "Interface" => "#8b5cf6".to_string(),
        "File" => "#64748b".to_string(),
        "Folder" => "#f59e0b".to_string(),
        "Module" | "Package" => "#10b981".to_string(),
        "Project" => "#f97316".to_string(),
        "Community" => community_color(community),
        "Process" => "#ec4899".to_string(),
        _ => community_color(community),
    }
}

fn node_size(node_type: &str) -> f64 {
    match node_type {
        "Project" => 18.0,
        "Package" | "Module" => 14.0,
        "Folder" => 12.0,
        "File" => 8.0,
        "Class" | "Interface" => 10.0,
        "Function" | "Method" => 7.0,
        "Community" => 20.0,
        "Process" => 16.0,
        _ => 6.0,
    }
}

// ── Main layout function ─────────────────────────────────────────────────────

/// Compute a force-directed layout for the given knowledge graph.
///
/// Takes nodes and edges from the ATree engine and produces 2D positions
/// using a parallel Barnes-Hut force simulation. The algorithm:
///
/// 1. Initialize positions using golden-angle radial placement (community-aware)
/// 2. For each iteration:
///    a. Build Barnes-Hut quadtree from current positions
///    b. Compute repulsion forces in parallel (work-stealing)
///    c. Compute spring forces for edges
///    d. Apply gravity toward center
///    e. Update velocities and positions with damping
/// 3. Return final positions as `GraphLayout`
pub fn compute_layout(
    nodes: &[atree_engine::graph::GraphNode],
    edges: &[atree_engine::graph::GraphEdge],
    config: &LayoutConfig,
) -> GraphLayout {
    match config.algorithm {
        LayoutAlgorithm::ForceDirected => compute_force_layout(nodes, edges, config),
        LayoutAlgorithm::LayeredDAG => compute_layered_layout(nodes, edges, config),
    }
}

/// Filter nodes and edges, returning dense-indexed data.
fn filter_graph<'a>(
    nodes: &'a [atree_engine::graph::GraphNode],
    edges: &'a [atree_engine::graph::GraphEdge],
    config: &LayoutConfig,
) -> (Vec<(usize, &'a atree_engine::graph::GraphNode)>, Vec<SimEdge>, FxHashMap<usize, usize>) {
    let node_filter_empty = config.node_type_filter.is_empty();
    let filtered_nodes: Vec<(usize, &atree_engine::graph::GraphNode)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| node_filter_empty || config.node_type_filter.contains(&n.label))
        .collect();

    let mut orig_to_dense: FxHashMap<usize, usize> = FxHashMap::default();
    for (dense, (orig, _)) in filtered_nodes.iter().enumerate() {
        orig_to_dense.insert(*orig, dense);
    }

    let edge_filter_empty = config.edge_filter.is_empty();
    let filtered_edges: Vec<SimEdge> = edges
        .iter()
        .filter(|e| edge_filter_empty || config.edge_filter.contains(&e.rel_type.as_str().to_string()))
        .filter_map(|e| {
            let source_dense = filtered_nodes.iter().position(|(_, n)| n.id == e.source_id)?;
            let target_dense = filtered_nodes.iter().position(|(_, n)| n.id == e.target_id)?;
            Some(SimEdge { source: source_dense, target: target_dense, strength: e.confidence })
        })
        .collect();

    (filtered_nodes, filtered_edges, orig_to_dense)
}

fn build_output_layout(
    filtered_nodes: &[(usize, &atree_engine::graph::GraphNode)],
    sim_nodes: &[SimNode],
    filtered_edges: &[SimEdge],
    edges: &[atree_engine::graph::GraphEdge],
    iterations: usize,
    elapsed_ms: u64,
) -> GraphLayout {
    let layout_nodes: Vec<LayoutNode> = filtered_nodes
        .iter()
        .enumerate()
        .map(|(i, (_, node))| {
            let sn = &sim_nodes[i];
            let community = node.properties.get("community_id").and_then(|v| v.parse::<usize>().ok());
            let depth = node.properties.get("depth").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
            let line = node.properties.get("line").and_then(|v| v.parse::<i64>().ok());
            LayoutNode {
                id: node.id.clone(),
                label: node.properties.get("name").cloned().unwrap_or_else(|| node.id.clone()),
                node_type: node.label.clone(),
                file_path: node.properties.get("file_path").cloned().unwrap_or_default(),
                line,
                x: sn.x,
                y: sn.y,
                size: node_size(&node.label),
                color: node_color(&node.label, community),
                community,
                depth,
            }
        })
        .collect();

    let layout_edges: Vec<LayoutEdge> = filtered_edges
        .iter()
        .map(|e| {
            let source_id = filtered_nodes[e.source].1.id.clone();
            let target_id = filtered_nodes[e.target].1.id.clone();
            let rel_type = edges
                .iter()
                .find(|oe| {
                    (oe.source_id == source_id && oe.target_id == target_id)
                        || (oe.source_id == target_id && oe.source_id == source_id)
                })
                .map(|oe| oe.rel_type.as_str().to_string())
                .unwrap_or_else(|| "UNKNOWN".to_string());
            LayoutEdge { source: source_id, target: target_id, rel_type, confidence: e.strength }
        })
        .collect();

    GraphLayout {
        nodes: layout_nodes,
        edges: layout_edges,
        stats: LayoutStats {
            node_count: filtered_nodes.len(),
            edge_count: filtered_edges.len(),
            iterations,
            elapsed_ms,
        },
    }
}

fn compute_force_layout(
    nodes: &[atree_engine::graph::GraphNode],
    edges: &[atree_engine::graph::GraphEdge],
    config: &LayoutConfig,
) -> GraphLayout {
    let start_time = std::time::Instant::now();

    let threads = if config.threads == 0 {
        atree_engine::half_cores().max(1)
    } else {
        config.threads.max(1)
    };

    let (filtered_nodes, _filtered_edges, _orig_to_dense) = filter_graph(nodes, edges, config);

    if filtered_nodes.is_empty() {
        return GraphLayout {
            nodes: vec![],
            edges: vec![],
            stats: LayoutStats { node_count: 0, edge_count: 0, iterations: 0, elapsed_ms: 0 },
        };
    }

    let node_count = filtered_nodes.len();

    // Build index mapping: original index → dense index
    let mut orig_to_dense: FxHashMap<usize, usize> = FxHashMap::default();
    for (dense, (orig, _)) in filtered_nodes.iter().enumerate() {
        orig_to_dense.insert(*orig, dense);
    }

    // Filter edges to only include filtered nodes
    let edge_filter_empty = config.edge_filter.is_empty();
    let filtered_edges: Vec<SimEdge> = edges
        .iter()
        .filter(|e| {
            edge_filter_empty || config.edge_filter.contains(&e.rel_type.as_str().to_string())
        })
        .filter_map(|e| {
            // Find dense indices for source and target
            let source_dense = filtered_nodes.iter().position(|(_, n)| n.id == e.source_id)?;
            let target_dense = filtered_nodes.iter().position(|(_, n)| n.id == e.target_id)?;
            Some(SimEdge {
                source: source_dense,
                target: target_dense,
                strength: e.confidence,
            })
        })
        .collect();

    // Initialize positions using golden-angle radial placement
    let mut rng = FastRng::new(config.seed);
    let spread = (node_count as f64).sqrt() * config.spring_length * 2.0;
    let golden_angle = std::f64::consts::PI * (3.0 - 5.0f64.sqrt());

    let mut sim_nodes: Vec<SimNode> = Vec::with_capacity(node_count);
    for (i, (_, node)) in filtered_nodes.iter().enumerate() {
        let angle = i as f64 * golden_angle;
        let radius = spread * ((i + 1) as f64 / node_count as f64).sqrt();
        let jitter = spread * 0.05;
        let x = radius * angle.cos() + (rng.next_f64() - 0.5) * jitter;
        let y = radius * angle.sin() + (rng.next_f64() - 0.5) * jitter;
        let community = node.properties.get("community_id").and_then(|v| v.parse::<usize>().ok());
        sim_nodes.push(SimNode {
            x, y, vx: 0.0, vy: 0.0,
            mass: node_size(&node.label) / 5.0,
            community,
        });
    }

    // Build adjacency for spring forces
    let mut adjacency: Vec<Vec<(usize, f64)>> = vec![Vec::new(); node_count];
    for edge in &filtered_edges {
        adjacency[edge.source].push((edge.target, edge.strength));
        adjacency[edge.target].push((edge.source, edge.strength));
    }

    // Simulation loop
    let damping = 0.9;
    let dt = 0.5;

    for _ in 0..config.iterations {
        // Compute bounding box
        let (min_x, min_y, max_x, max_y) = if node_count > 100 {
            let mut min_x = f64::MAX;
            let mut min_y = f64::MAX;
            let mut max_x = f64::MIN;
            let mut max_y = f64::MIN;
            for sn in &sim_nodes {
                min_x = min_x.min(sn.x);
                min_y = min_y.min(sn.y);
                max_x = max_x.max(sn.x);
                max_y = max_y.max(sn.y);
            }
            let padding = (max_x - min_x).max(max_y - min_y) * 0.1 + 1.0;
            (min_x - padding, min_y - padding, max_x + padding, max_y + padding)
        } else {
            (-spread, -spread, spread, spread)
        };
        let size = (max_x - min_x).max(max_y - min_y);

        // Build quadtree
        let mut qt = QuadTree::new();
        for sn in &sim_nodes {
            qt.insert(sn.x, sn.y, sn.mass, min_x, min_y, size);
        }

        // Compute forces in parallel
        let forces: Vec<(f64, f64)> = if node_count > 500 && threads > 1 {
            let chunk_size = (node_count + threads - 1) / threads;
            let sim_nodes_ref = &sim_nodes;
            let adjacency_ref = &adjacency;
            let qt_ref = &qt;

            let results: Vec<Vec<(f64, f64)>> = thread::scope(|s| {
                let handles: Vec<_> = (0..threads)
                    .map(|t| {
                        let start = t * chunk_size;
                        let end = ((t + 1) * chunk_size).min(node_count);
                        s.spawn(move || {
                            let mut local_forces = Vec::with_capacity(end - start);
                            for i in start..end {
                                let sn = &sim_nodes_ref[i];
                                let (mut fx, mut fy) = qt_ref.compute_force(sn.x, sn.y, config.theta, config.repulsion);

                                // Spring forces
                                for (neighbor, strength) in &adjacency_ref[i] {
                                    let other = &sim_nodes_ref[*neighbor];
                                    let dx = other.x - sn.x;
                                    let dy = other.y - sn.y;
                                    let dist = (dx * dx + dy * dy).sqrt() + 1.0;
                                    let displacement = dist - config.spring_length;
                                    let spring_force = config.spring_strength * displacement * strength;
                                    fx += spring_force * dx / dist;
                                    fy += spring_force * dy / dist;
                                }

                                // Gravity toward center
                                fx -= config.gravity * sn.x;
                                fy -= config.gravity * sn.y;

                                local_forces.push((fx, fy));
                            }
                            local_forces
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            results.into_iter().flatten().collect()
        } else {
            // Sequential for small graphs
            let mut forces = Vec::with_capacity(node_count);
            for i in 0..node_count {
                let sn = &sim_nodes[i];
                let (mut fx, mut fy) = qt.compute_force(sn.x, sn.y, config.theta, config.repulsion);
                for (neighbor, strength) in &adjacency[i] {
                    let other = &sim_nodes[*neighbor];
                    let dx = other.x - sn.x;
                    let dy = other.y - sn.y;
                    let dist = (dx * dx + dy * dy).sqrt() + 1.0;
                    let displacement = dist - config.spring_length;
                    let spring_force = config.spring_strength * displacement * strength;
                    fx += spring_force * dx / dist;
                    fy += spring_force * dy / dist;
                }
                fx -= config.gravity * sn.x;
                fy -= config.gravity * sn.y;
                forces.push((fx, fy));
            }
            forces
        };

        // Update velocities and positions
        for i in 0..node_count {
            sim_nodes[i].vx = (sim_nodes[i].vx + forces[i].0 * dt) * damping;
            sim_nodes[i].vy = (sim_nodes[i].vy + forces[i].1 * dt) * damping;
            sim_nodes[i].x += sim_nodes[i].vx * dt;
            sim_nodes[i].y += sim_nodes[i].vy * dt;
        }
    }

    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    build_output_layout(&filtered_nodes, &sim_nodes, &filtered_edges, edges, config.iterations, elapsed_ms)
}

// ── Layered DAG Layout (Sugiyama-style) ─────────────────────────────────────

/// Compute a layered DAG layout.
///
/// Algorithm (simplified Sugiyama):
/// 1. Build directed graph from edges (use direction if available, otherwise bidirectional)
/// 2. Assign layers via longest-path layering (minimize edge length)
/// /// 3. Add dummy nodes for edges that span multiple layers
/// 4. Order nodes within each layer to minimize crossings (barycenter heuristic)
/// 5. Assign coordinates based on layer position and within-layer order
///
/// The result shows flow direction: upstream nodes on the left, downstream on the right.
fn compute_layered_layout(
    nodes: &[atree_engine::graph::GraphNode],
    edges: &[atree_engine::graph::GraphEdge],
    config: &LayoutConfig,
) -> GraphLayout {
    let start_time = std::time::Instant::now();
    let (filtered_nodes, filtered_edges, _orig_to_dense) = filter_graph(nodes, edges, config);

    if filtered_nodes.is_empty() {
        return GraphLayout {
            nodes: vec![],
            edges: vec![],
            stats: LayoutStats { node_count: 0, edge_count: 0, iterations: 0, elapsed_ms: 0 },
        };
    }

    let node_count = filtered_nodes.len();
    let layer_spacing = config.layer_spacing;
    let node_spacing = config.node_spacing;

    // Step 1: Build adjacency (directed: source → target based on edge direction)
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); node_count];
    let mut in_degree: Vec<usize> = vec![0; node_count];
    for edge in &filtered_edges {
        adj[edge.source].push(edge.target);
        in_degree[edge.target] += 1;
    }

    // Step 2: Longest-path layering
    // Start from source nodes (in_degree == 0), assign layer = longest path from any source
    let mut layers: Vec<i32> = vec![-1; node_count];
    let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();

    for i in 0..node_count {
        if in_degree[i] == 0 {
            layers[i] = 0;
            queue.push_back(i);
        }
    }

    // If no pure sources (cycle), start from all nodes with minimum in_degree
    if queue.is_empty() {
        let min_deg = *in_degree.iter().min().unwrap_or(&0);
        for i in 0..node_count {
            if in_degree[i] == min_deg {
                layers[i] = 0;
                queue.push_back(i);
            }
        }
    }

    // If still empty, start from node 0
    if queue.is_empty() && node_count > 0 {
        layers[0] = 0;
        queue.push_back(0);
    }

    // Topological longest-path via Kahn's algorithm
    let mut temp_in = in_degree.clone();
    while let Some(u) = queue.pop_front() {
        for &v in &adj[u] {
            let new_layer = layers[u] + 1;
            if new_layer > layers[v] {
                layers[v] = new_layer;
            }
            temp_in[v] -= 1;
            if temp_in[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    // Handle unreachable nodes (in cycles) — assign to layer 0
    for i in 0..node_count {
        if layers[i] < 0 {
            layers[i] = 0;
        }
    }

    // Step 3: Group nodes by layer
    let max_layer = *layers.iter().max().unwrap_or(&0);
    let layer_count = (max_layer + 1) as usize;
    let mut layer_nodes: Vec<Vec<usize>> = vec![Vec::new(); layer_count];
    for i in 0..node_count {
        layer_nodes[layers[i] as usize].push(i);
    }

    // Step 4: Order nodes within each layer to minimize crossings (barycenter heuristic)
    // For each node, compute barycenter = average position of neighbors in previous layer
    // Sort by barycenter to reduce edge crossings
    for iteration in 0..4 { // Multiple passes for better results
        let forward = iteration % 2 == 0;

        if forward {
            for l in 1..layer_count {
                let prev_layer = &layer_nodes[l - 1];
                if prev_layer.is_empty() { continue; }

                // Build position map for previous layer
                let prev_positions: FxHashMap<usize, usize> = prev_layer
                    .iter()
                    .enumerate()
                    .map(|(pos, &node)| (node, pos))
                    .collect();

                // Compute barycenter for each node in current layer
                let mut barycenters: Vec<(usize, f64)> = layer_nodes[l]
                    .iter()
                    .map(|&node| {
                        let neighbors: Vec<usize> = adj[node].iter()
                            .filter(|&&n| layers[n] as usize == l - 1)
                            .copied()
                            .collect();
                        if neighbors.is_empty() {
                            return (node, node as f64); // fallback: use index
                        }
                        let sum: f64 = neighbors.iter()
                            .filter_map(|n| prev_positions.get(n))
                            .map(|&pos| pos as f64)
                            .sum();
                        (node, sum / neighbors.len() as f64)
                    })
                    .collect();

                barycenters.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                layer_nodes[l] = barycenters.into_iter().map(|(node, _)| node).collect();
            }
        } else {
            // Backward pass
            for l in (0..layer_count.saturating_sub(1)).rev() {
                let next_layer = &layer_nodes[l + 1];
                if next_layer.is_empty() { continue; }

                let next_positions: FxHashMap<usize, usize> = next_layer
                    .iter()
                    .enumerate()
                    .map(|(pos, &node)| (node, pos))
                    .collect();

                let mut barycenters: Vec<(usize, f64)> = layer_nodes[l]
                    .iter()
                    .map(|&node| {
                        let neighbors: Vec<usize> = adj[node].iter()
                            .filter(|&&n| layers[n] as usize == l + 1)
                            .copied()
                            .collect();
                        if neighbors.is_empty() {
                            return (node, node as f64);
                        }
                        let sum: f64 = neighbors.iter()
                            .filter_map(|n| next_positions.get(n))
                            .map(|&pos| pos as f64)
                            .sum();
                        (node, sum / neighbors.len() as f64)
                    })
                    .collect();

                barycenters.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                layer_nodes[l] = barycenters.into_iter().map(|(node, _)| node).collect();
            }
        }
    }

    // Step 5: Assign coordinates
    // x = layer * layer_spacing, y = position_in_layer * node_spacing (centered)
    let mut positions: Vec<(f64, f64)> = vec![(0.0, 0.0); node_count];
    for (layer_idx, layer) in layer_nodes.iter().enumerate() {
        let x = layer_idx as f64 * layer_spacing;
        let layer_height = layer.len() as f64 * node_spacing;
        let y_offset = -layer_height / 2.0; // center vertically
        for (pos_in_layer, &node_idx) in layer.iter().enumerate() {
            let y = y_offset + pos_in_layer as f64 * node_spacing;
            positions[node_idx] = (x, y);
        }
    }

    // Apply jitter to separate coincident nodes (deterministic based on seed)
    let mut rng = FastRng::new(config.seed);
    for i in 0..node_count {
        let jitter_x = (rng.next_f64() - 0.5) * node_spacing * 0.1;
        let jitter_y = (rng.next_f64() - 0.5) * node_spacing * 0.1;
        positions[i].0 += jitter_x;
        positions[i].1 += jitter_y;
    }

    // Build sim_nodes from positions for output
    let sim_nodes: Vec<SimNode> = positions.iter().map(|(x, y)| {
        SimNode { x: *x, y: *y, vx: 0.0, vy: 0.0, mass: 1.0, community: None }
    }).collect();

    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    build_output_layout(&filtered_nodes, &sim_nodes, &filtered_edges, edges, 0, elapsed_ms)
}
