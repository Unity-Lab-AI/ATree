//! Overload narrowing — select the best matching overload from candidates
//! based on arity and argument types.

use crate::semantic::Symbol;

/// Narrow overload candidates by arity and argument types.
/// Returns the best matching candidates (may be multiple if ambiguous).
pub fn narrow_overload_candidates<'a>(
    candidates: &'a [Symbol],
    arity: Option<usize>,
    _argument_types: Option<&[String]>,
) -> Vec<&'a Symbol> {
    if candidates.len() <= 1 {
        return candidates.iter().collect();
    }

    // First, filter by arity if available
    if let Some(_arg_count) = arity {
        let arity_matches: Vec<&Symbol> = candidates.iter()
            .filter(|_sym| {
                // For now, we can't easily determine arity from the symbol alone
                // without parameter extraction. Accept all candidates.
                true
            })
            .collect();
        if !arity_matches.is_empty() {
            return arity_matches;
        }
    }

    // Argument type matching: narrow candidates by comparing parameter types
    // from the call site's type environment against the candidate's parameter types.
    // For now, return all arity-matched candidates (type matching requires cross-file
    // type environment which is populated by build_type_env).
    candidates.iter().collect()
}
