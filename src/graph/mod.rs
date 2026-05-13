use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use crate::resolver::{SemanticModel, EdgeKind};

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

    /// Build a rich code graph from the SemanticModel.
    /// Nodes: files, symbols, calls, imports, heritage, assignments, decorators, http_clients.
    /// Edges: defines, calls (with resolution), extends, implements, imports, etc.
    pub fn from_model(model: &SemanticModel) -> Self {
        let mut graph = Self::new();

        // File nodes + symbol nodes + defines edges
        for (file_id, file) in &model.files {
            let file_node_id = format!("file:{}", file_id);
            let mut file_props = HashMap::new();
            file_props.insert("language".to_string(), format!("{:?}", file.language));
            file_props.insert("path".to_string(), file.path.clone());
            graph.add_node(file_node_id.clone(), file.path.clone(), "file".to_string(), file_props);

            // Symbol nodes
            for sym in &file.symbols {
                let sym_node_id = format!("sym:{}", sym.id);
                let mut sym_props = HashMap::new();
                sym_props.insert("kind".to_string(), format!("{:?}", sym.kind));
                sym_props.insert("line".to_string(), sym.line.to_string());
                sym_props.insert("col".to_string(), sym.col.to_string());
                sym_props.insert("qualified_name".to_string(), sym.qualified_name.clone());
                if sym.is_exported {
                    sym_props.insert("exported".to_string(), "true".to_string());
                }
                graph.add_node(sym_node_id.clone(), sym.name.clone(), "symbol".to_string(), sym_props);
                graph.add_edge(file_node_id.clone(), sym_node_id.clone(), "defines".to_string(), 1.0);
            }

            // Call nodes with resolution
            for call in &file.calls {
                let call_node_id = format!("call:{}:{}:{}", file_id, call.callee_name, call.line);
                let mut call_props = HashMap::new();
                call_props.insert("line".to_string(), call.line.to_string());
                call_props.insert("confidence".to_string(), format!("{:?}", call.confidence));
                call_props.insert("confidence_score".to_string(), call.confidence.score().to_string());
                if let Some(ref recv) = call.receiver {
                    call_props.insert("receiver".to_string(), recv.clone());
                }
                graph.add_node(call_node_id.clone(), call.callee_name.clone(), "call".to_string(), call_props);
                graph.add_edge(file_node_id.clone(), call_node_id.clone(), "contains".to_string(), 1.0);

                // Resolution edge: call → symbol
                if let Some(resolved_id) = call.resolved_symbol_id {
                    let target = format!("sym:{}", resolved_id);
                    graph.add_edge(call_node_id.clone(), target, "resolves_to".to_string(), call.confidence.score());
                }
            }

            // Import nodes
            for imp in &file.imports {
                let imp_node_id = format!("import:{}:{}", file_id, imp.source);
                let mut imp_props = HashMap::new();
                imp_props.insert("source".to_string(), imp.source.clone());
                imp_props.insert("imported_name".to_string(), imp.imported_name.clone());
                imp_props.insert("local_name".to_string(), imp.local_name.clone());
                imp_props.insert("confidence".to_string(), format!("{:?}", imp.confidence));
                graph.add_node(imp_node_id.clone(), imp.local_name.clone(), "import".to_string(), imp_props);
                graph.add_edge(file_node_id.clone(), imp_node_id.clone(), "imports".to_string(), imp.confidence.score());

                // Link import to resolved file
                if let Some(target_file) = imp.resolved_file_id {
                    let target = format!("file:{}", target_file);
                    graph.add_edge(imp_node_id.clone(), target, "resolves_to".to_string(), imp.confidence.score());
                }
            }

            // Heritage nodes
            for h in &file.heritage {
                let h_node_id = format!("heritage:{}:{}:{}", file_id, h.target_name, h.line);
                let mut h_props = HashMap::new();
                h_props.insert("kind".to_string(), format!("{:?}", h.heritage_kind));
                h_props.insert("line".to_string(), h.line.to_string());
                h_props.insert("confidence".to_string(), format!("{:?}", h.confidence));
                graph.add_node(h_node_id.clone(), h.target_name.clone(), "heritage".to_string(), h_props);

                let edge_type = match h.heritage_kind {
                    crate::semantic::HeritageKind::Extends => "extends",
                    crate::semantic::HeritageKind::Implements => "implements",
                    crate::semantic::HeritageKind::UsesTrait => "uses_trait",
                    _ => "heritage",
                };
                graph.add_edge(file_node_id.clone(), h_node_id.clone(), edge_type.to_string(), h.confidence.score());

                if let Some(resolved_id) = h.resolved_symbol_id {
                    let target = format!("sym:{}", resolved_id);
                    graph.add_edge(h_node_id.clone(), target, "resolves_to".to_string(), h.confidence.score());
                }
            }

            // Assignment nodes
            for a in &file.assignments {
                let a_node_id = format!("assign:{}:{}:{}", file_id, a.name, a.line);
                let mut a_props = HashMap::new();
                a_props.insert("line".to_string(), a.line.to_string());
                if let Some(ref recv) = a.receiver {
                    a_props.insert("receiver".to_string(), recv.clone());
                }
                graph.add_node(a_node_id.clone(), a.name.clone(), "assignment".to_string(), a_props);
                graph.add_edge(file_node_id.clone(), a_node_id, "writes".to_string(), 1.0);
            }

            // Decorator nodes
            for d in &file.decorators {
                let d_node_id = format!("decorator:{}:{}:{}", file_id, d.name, d.line);
                let mut d_props = HashMap::new();
                d_props.insert("line".to_string(), d.line.to_string());
                graph.add_node(d_node_id.clone(), d.name.clone(), "decorator".to_string(), d_props);
                graph.add_edge(file_node_id.clone(), d_node_id, "decorates".to_string(), 1.0);
            }

            // HTTP client nodes
            for h in &file.http_clients {
                let h_node_id = format!("http:{}:{}:{}", file_id, h.name, h.line);
                let mut h_props = HashMap::new();
                h_props.insert("line".to_string(), h.line.to_string());
                if let Some(ref url) = h.url {
                    h_props.insert("url".to_string(), url.clone());
                }
                graph.add_node(h_node_id.clone(), h.name.clone(), "http_client".to_string(), h_props);
                graph.add_edge(file_node_id.clone(), h_node_id, "http_request".to_string(), 1.0);
            }
        }

        // Cross-file resolution edges from the model
        for edge in &model.edges {
            let src_id = if edge.src_id == 0 {
                // Call-site edge — source is already linked via file contains
                continue;
            } else {
                format!("sym:{}", edge.src_id)
            };
            let dst_id = format!("sym:{}", edge.dst_id);
            let edge_type = match edge.edge_kind {
                EdgeKind::Calls => "calls",
                EdgeKind::Extends => "extends",
                EdgeKind::Implements => "implements",
                EdgeKind::UsesTrait => "uses_trait",
                EdgeKind::Imports => "imports",
                EdgeKind::Exports => "exports",
                EdgeKind::References => "references",
                EdgeKind::Assigns => "assigns",
                EdgeKind::Decorates => "decorates",
                EdgeKind::HttpRequest => "http_request",
            };
            graph.add_edge(src_id, dst_id, edge_type.to_string(), edge.confidence.score());
        }

        graph
    }

    fn add_node(&mut self, id: String, label: String, node_type: String, props: HashMap<String, String>) {
        if !self.nodes.iter().any(|n| n.id == id) {
            self.nodes.push(CodeNode { id, label, node_type, properties: props });
        }
    }

    fn add_edge(&mut self, source: String, target: String, edge_type: String, confidence: f64) {
        let id = format!("{}-{}-{}-{}", source, target, edge_type, confidence);
        if !self.edges.iter().any(|e| e.id == id) {
            self.edges.push(CodeEdge { id, source, target, edge_type, confidence });
        }
    }
}
