//! Data flow analysis — track value propagation through assignments, parameter
//! passing, returns, and property access.
//!
//! This module analyzes assignment patterns, function parameter passing, return
//! value flows, and property reads/writes to build a data flow graph that
//! complements the call graph.

use crate::store::{GraphStore, DataFlowRecord};
use rustc_hash::FxHashMap;


/// Extract data flow edges directly from the parsed file assignments and store them.
/// This is called from the pipeline with resolved symbol/file IDs.
pub fn extract_and_store_flows(
    store: &GraphStore,
    file_id: i64,
    assignments: &[crate::semantic::Assignment],
    calls: &[crate::semantic::Call],
    type_bindings: &[crate::syntax::TypeBinding],
    symbols: &[crate::store::SymbolRecord],
    _symbol_id_map: &FxHashMap<u64, i64>,
) -> rusqlite::Result<usize> {
    let mut records = Vec::new();

    // Build name→symbol_id lookup for this file
    let sym_by_name: FxHashMap<&str, i64> = symbols.iter()
        .filter(|s| s.file_id == file_id)
        .map(|s| (s.name.as_str(), s.id))
        .collect();

    // Extract property_write / property_read from assignments with receivers
    for assignment in assignments {
        if let Some(ref receiver) = assignment.receiver {
            // obj.field = value → property_write
            let dst = sym_by_name.get(assignment.name.as_str()).copied().unwrap_or(0);
            let src = sym_by_name.get(receiver.as_str()).copied().unwrap_or(0);
            if src > 0 && dst > 0 {
                records.push(DataFlowRecord {
                    id: 0, file_id, src_symbol_id: src, dst_symbol_id: dst,
                    flow_kind: "property_write".to_string(),
                    var_name: assignment.name.clone(),
                    line: assignment.line, col: 0,
                    confidence: 0.90,
                });
                // Also emit ACCESSES_write edge
                let _ = store.insert_access_edge(src, dst, "write", 0.90, file_id, assignment.line as i64);
            }
        } else {
            // Simple assignment: name = value → assignment flow
            if let Some(&dst) = sym_by_name.get(assignment.name.as_str()) {
                records.push(DataFlowRecord {
                    id: 0, file_id,
                    src_symbol_id: 0,
                    dst_symbol_id: dst,
                    flow_kind: "assignment".to_string(),
                    var_name: assignment.name.clone(),
                    line: assignment.line, col: 0,
                    confidence: 0.85,
                });
            }
        }
    }

    // Extract param_pass flows from calls
    for call in calls {
        if let Some(ref receiver) = call.receiver {
            // obj.method() → property_read on obj
            let dst = sym_by_name.get(call.callee_name.as_str()).copied().unwrap_or(0);
            let src = sym_by_name.get(receiver.as_str()).copied().unwrap_or(0);
            if src > 0 && dst > 0 {
                let _ = store.insert_access_edge(src, dst, "read", 0.85, file_id, call.line as i64);
            }
        }
        // Every call implies param_pass
        if let Some(&callee_id) = sym_by_name.get(call.callee_name.as_str()) {
            records.push(DataFlowRecord {
                id: 0, file_id,
                src_symbol_id: 0,
                dst_symbol_id: callee_id,
                flow_kind: "param_pass".to_string(),
                var_name: call.callee_name.clone(),
                line: call.line, col: call.col,
                confidence: 0.80,
            });
        }
    }

    // Extract type bindings as data flows
    for binding in type_bindings {
        if let Some(&dst) = sym_by_name.get(binding.var_name.as_str()) {
            records.push(DataFlowRecord {
                id: 0, file_id,
                src_symbol_id: 0,
                dst_symbol_id: dst,
                flow_kind: "assignment".to_string(),
                var_name: binding.var_name.clone(),
                line: binding.line, col: 0,
                confidence: 0.80,
            });
        }
    }

    if records.is_empty() {
        return Ok(0);
    }

    store.insert_data_flows_batch(&records)
}

/// Trace the full data flow from a variable through the program.
#[allow(dead_code)]
pub fn trace_value_flow(
    store: &GraphStore,
    symbol_id: i64,
    max_depth: usize,
) -> rusqlite::Result<Vec<(i64, String, i64)>> {
    store.trace_data_flow_forward(symbol_id, max_depth as i64)
}
