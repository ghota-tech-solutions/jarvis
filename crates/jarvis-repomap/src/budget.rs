//! Greedy token-budget selector.
//!
//! Entries are assumed already sorted (highest score first). We pick them in
//! order while their cumulative estimated token cost fits the budget.

use crate::MapEntry;

/// Estimate the token cost of an entry. Mirrors the heuristic from Aider:
/// roughly `chars / 3` plus a small constant per entry for formatting
/// (file separator, line number, etc).
pub(crate) fn estimate_entry_tokens(e: &MapEntry) -> usize {
    let chars = e.symbol.len() + e.signature.len() + e.file.to_string_lossy().len();
    chars / 3 + 4
}

/// Greedy budgeting. Returns the truncated list and the cumulative token
/// estimate of the kept entries.
pub(crate) fn truncate_to_budget(entries: Vec<MapEntry>, budget: usize) -> (Vec<MapEntry>, usize) {
    let mut kept = Vec::with_capacity(entries.len());
    let mut total = 0usize;
    for e in entries {
        let cost = estimate_entry_tokens(&e);
        if total + cost > budget {
            // Stop on the first overflow — matches Aider's behaviour of
            // preserving the highest-ranked prefix instead of cherry-picking
            // smaller entries to squeeze in.
            break;
        }
        total += cost;
        kept.push(e);
    }
    (kept, total)
}
