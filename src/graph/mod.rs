use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodeNode {
    pub id: String,
    pub label: String,
    pub properties: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CodeEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub edge_type: String,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct CodeGraph {
    pub nodes: Vec<CodeNode>,
    pub edges: Vec<CodeEdge>,
}

impl CodeGraph {
    pub fn add_node(&mut self, id: String, label: String, props: HashMap<String, String>) {
        self.nodes.push(CodeNode { id, label, properties: props });
    }

    pub fn add_edge(&mut self, source: String, target: String, edge_type: String) {
        let id = format!("{}-{}-{}", source, target, edge_type);
        self.edges.push(CodeEdge { id, source, target, edge_type });
    }
}
