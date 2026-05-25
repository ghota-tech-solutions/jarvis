//! Tree-sitter based repo map with PageRank ranking and token budgeting.
//!
//! This crate produces a compact symbolic view of a source tree, suitable
//! for being injected as default context into an LLM agent. The design is
//! inspired by Aider's `repomap.py`:
//!
//! 1. Walk the repo (honouring `.gitignore`) and parse each supported file
//!    via tree-sitter.
//! 2. Extract symbol *definitions* (functions, types, traits, ...) and
//!    symbol *references* (call sites, identifier uses).
//! 3. Build a directed graph file -> referenced-symbol and run a few
//!    iterations of PageRank to score symbols by global importance.
//! 4. Greedily pick the top-N entries that fit in a token budget.
//!
//! The result is a [`RepoMap`] with sorted, budgeted entries ready to be
//! rendered as plain text in a system prompt.

use std::path::PathBuf;

use thiserror::Error;

mod budget;
mod extractor;
mod graph;

pub use extractor::SymbolKind;

/// Errors produced while building a repo map.
#[derive(Debug, Error)]
pub enum RepoMapError {
    /// The root path passed in [`RepoMapOptions`] does not exist.
    #[error("root path does not exist: {0}")]
    RootMissing(PathBuf),
    /// Underlying I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Failure while configuring a tree-sitter query.
    #[error("tree-sitter query error: {0}")]
    Query(String),
}

/// One entry in the rendered repo map.
#[derive(Debug, Clone)]
pub struct MapEntry {
    /// File the symbol is defined in, relative to the repo root.
    pub file: PathBuf,
    /// Symbol name (the identifier as written in source).
    pub symbol: String,
    /// Kind of symbol (function / type / trait / ...).
    pub kind: SymbolKind,
    /// 1-based line number of the definition.
    pub line: u32,
    /// Short signature, truncated to ~120 characters.
    pub signature: String,
    /// PageRank score (higher = more important).
    pub score: f64,
}

/// Options for building a repo map.
#[derive(Debug, Clone)]
pub struct RepoMapOptions {
    /// Repo root to walk.
    pub root: PathBuf,
    /// Approximate budget in tokens (chars / 3 heuristic).
    pub token_budget: usize,
    /// Extra globs to ignore on top of `.gitignore`.
    pub extra_ignore: Vec<String>,
}

impl Default for RepoMapOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            token_budget: 1024,
            extra_ignore: Vec::new(),
        }
    }
}

/// The output of [`build_repomap`].
#[derive(Debug, Clone)]
pub struct RepoMap {
    /// Entries sorted by PageRank score (descending), already truncated to
    /// fit `token_budget`.
    pub entries: Vec<MapEntry>,
    /// Total number of source files that were parsed.
    pub total_files_scanned: usize,
    /// Sum of estimated token sizes for `entries`.
    pub token_estimate: usize,
}

/// Build a repo map for `opts.root`.
pub fn build_repomap(opts: &RepoMapOptions) -> Result<RepoMap, RepoMapError> {
    if !opts.root.exists() {
        return Err(RepoMapError::RootMissing(opts.root.clone()));
    }

    let extraction = extractor::walk_and_extract(&opts.root, &opts.extra_ignore)?;
    let scores = graph::pagerank(&extraction);
    let mut entries = extractor::into_entries(extraction.definitions, &scores);

    // Stable sort by descending score, tie-break by file/line so the output
    // is deterministic.
    entries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    let (entries, token_estimate) = budget::truncate_to_budget(entries, opts.token_budget);

    Ok(RepoMap {
        entries,
        total_files_scanned: extraction.files_scanned,
        token_estimate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_empty_dir_returns_empty_map() {
        let dir = TempDir::new().unwrap();
        let map = build_repomap(&RepoMapOptions {
            root: dir.path().to_path_buf(),
            token_budget: 1024,
            extra_ignore: vec![],
        })
        .unwrap();
        assert!(map.entries.is_empty());
        assert_eq!(map.total_files_scanned, 0);
        assert_eq!(map.token_estimate, 0);
    }

    #[test]
    fn test_rust_function_extraction() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("lib.rs"), "pub fn foo() -> i32 { 42 }\n").unwrap();
        let map = build_repomap(&RepoMapOptions {
            root: dir.path().to_path_buf(),
            token_budget: 1024,
            extra_ignore: vec![],
        })
        .unwrap();
        assert_eq!(map.total_files_scanned, 1);
        let foo = map
            .entries
            .iter()
            .find(|e| e.symbol == "foo")
            .expect("foo entry");
        assert_eq!(foo.kind, SymbolKind::Function);
        assert!(foo.signature.contains("foo"));
    }

    #[test]
    fn test_budget_truncates() {
        // Build a small synthetic project with several functions so the
        // budget can actually bite.
        let dir = TempDir::new().unwrap();
        let mut src = String::new();
        for i in 0..50 {
            src.push_str(&format!(
                "pub fn function_with_a_longish_name_{i}() {{ function_with_a_longish_name_{prev}(); }}\n",
                prev = (i + 1) % 50
            ));
        }
        fs::write(dir.path().join("lib.rs"), src).unwrap();

        let map = build_repomap(&RepoMapOptions {
            root: dir.path().to_path_buf(),
            token_budget: 64,
            extra_ignore: vec![],
        })
        .unwrap();
        // Total estimate should respect (and stay close to) the budget.
        assert!(
            map.token_estimate <= 100,
            "token_estimate={} exceeded margin over budget=64",
            map.token_estimate
        );
        // Make sure we actually picked at least one entry.
        assert!(!map.entries.is_empty());
        // ... and that not all 50 entries made it in.
        assert!(map.entries.len() < 50);
    }

    #[test]
    fn test_entries_sorted_by_score_descending() {
        let dir = TempDir::new().unwrap();
        // `popular` is called by many; should outrank `lonely`.
        let src = r#"
pub fn popular() {}
pub fn lonely() {}
pub fn a() { popular(); }
pub fn b() { popular(); }
pub fn c() { popular(); }
"#;
        fs::write(dir.path().join("lib.rs"), src).unwrap();
        let map = build_repomap(&RepoMapOptions {
            root: dir.path().to_path_buf(),
            token_budget: 4096,
            extra_ignore: vec![],
        })
        .unwrap();
        let mut prev = f64::INFINITY;
        for e in &map.entries {
            assert!(e.score <= prev + 1e-9, "entries are not sorted");
            prev = e.score;
        }
        let popular_score = map
            .entries
            .iter()
            .find(|e| e.symbol == "popular")
            .map(|e| e.score)
            .unwrap();
        let lonely_score = map
            .entries
            .iter()
            .find(|e| e.symbol == "lonely")
            .map(|e| e.score)
            .unwrap();
        assert!(popular_score > lonely_score);
    }
}
