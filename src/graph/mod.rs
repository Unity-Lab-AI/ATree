//! Code graph types for JSON output.
//!
//! The actual graph is stored in SQLite via GraphStore.
//! These types are for serializing query results.

use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodeNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub properties: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodeEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub confidence: f64,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct CodeGraph {
    pub nodes: Vec<CodeNode>,
    pub edges: Vec<CodeEdge>,
}

impl CodeGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_node(&mut self, id: String, label: String, node_type: String, props: HashMap<String, String>) {
        if !self.nodes.iter().any(|n| n.id == id) {
            self.nodes.push(CodeNode { id, label, node_type, properties: props });
        }
    }

    pub fn add_edge(&mut self, source: String, target: String, edge_type: String, confidence: f64) {
        let id = format!("{}-{}-{}-{}", source, target, edge_type, confidence);
        if !self.edges.iter().any(|e| e.id == id) {
            self.edges.push(CodeEdge { id, source, target, edge_type, confidence });
        }
    }
}
