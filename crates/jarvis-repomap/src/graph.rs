//! Directed graph + PageRank over (file, symbol) nodes.
//!
//! The graph is built from two pieces of information collected by the
//! extractor:
//!
//! * `definitions`: every symbol definition, identified by `file::symbol`.
//! * `references_by_file`: per file, the list of identifier names that are
//!   used in that file's source.
//!
//! We add an edge from every definition node located in file `F` to every
//! definition node whose symbol name appears in `references_by_file[F]`.
//! This is intentionally coarse — Aider does the same — and PageRank lets
//! the noise wash out.

use std::collections::HashMap;

use crate::extractor::{Definition, Extraction};

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 30;

/// Compute PageRank scores over the extracted graph. Returns a map of
/// `node_id` (`file::symbol`) -> score.
pub(crate) fn pagerank(extraction: &Extraction) -> HashMap<String, f64> {
    let nodes: Vec<&Definition> = extraction.definitions.iter().collect();
    let n = nodes.len();
    if n == 0 {
        return HashMap::new();
    }

    // Index symbols by name → list of node indices that define them.
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, d) in nodes.iter().enumerate() {
        by_name.entry(d.symbol.as_str()).or_default().push(i);
    }
    // Index nodes by file → indices of definitions in that file (so edges
    // out of "file F" can be expanded to "every definition in F").
    let mut by_file: HashMap<&std::path::Path, Vec<usize>> = HashMap::new();
    for (i, d) in nodes.iter().enumerate() {
        by_file.entry(d.file.as_path()).or_default().push(i);
    }

    // Build out-adjacency. Edge (src -> dst) when:
    //   * src is a definition in file F,
    //   * F references a symbol whose name matches dst.symbol,
    //   * src != dst (no self-loops; a file referencing its own symbol is
    //     uninteresting and just creates a fixed point).
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (file, refs) in &extraction.references_by_file {
        let Some(src_indices) = by_file.get(file.as_path()) else {
            continue;
        };
        for r in refs {
            let Some(dst_indices) = by_name.get(r.as_str()) else {
                continue;
            };
            for &src in src_indices {
                for &dst in dst_indices {
                    if src != dst {
                        out[src].push(dst);
                    }
                }
            }
        }
    }
    // Dedup outgoing edges per source to make PageRank distribute weight
    // more sensibly across distinct targets.
    for v in out.iter_mut() {
        v.sort_unstable();
        v.dedup();
    }

    // Standard PageRank iteration.
    let mut rank = vec![1.0 / n as f64; n];
    let base = (1.0 - DAMPING) / n as f64;
    for _ in 0..ITERATIONS {
        let mut next = vec![base; n];
        // Distribute dangling-node mass uniformly so the total stays at 1.
        let mut dangling_mass = 0.0_f64;
        for i in 0..n {
            if out[i].is_empty() {
                dangling_mass += rank[i];
            }
        }
        let dangling_share = DAMPING * dangling_mass / n as f64;
        for entry in next.iter_mut() {
            *entry += dangling_share;
        }
        for i in 0..n {
            if out[i].is_empty() {
                continue;
            }
            let share = DAMPING * rank[i] / out[i].len() as f64;
            for &dst in &out[i] {
                next[dst] += share;
            }
        }
        rank = next;
    }

    nodes
        .iter()
        .enumerate()
        .map(|(i, d)| (d.node_id(), rank[i]))
        .collect()
}
