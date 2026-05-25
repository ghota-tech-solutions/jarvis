//! End-to-end test: build a repo map on the jarvis workspace itself.

use std::path::PathBuf;

use jarvis_repomap::{RepoMapOptions, build_repomap};

/// Walk up from this crate's manifest to find the workspace root.
fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` points at `crates/jarvis-repomap` at test time.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .expect("workspace root")
}

#[test]
fn test_build_repomap_on_self() {
    let root = workspace_root();
    let map = build_repomap(&RepoMapOptions {
        root: root.clone(),
        token_budget: 8192,
        extra_ignore: vec![],
    })
    .expect("repomap build");

    assert!(
        map.total_files_scanned > 10,
        "expected to scan more than 10 files, got {}",
        map.total_files_scanned
    );
    assert!(!map.entries.is_empty(), "no entries extracted");

    // Sorted by score descending.
    let mut prev = f64::INFINITY;
    for e in &map.entries {
        assert!(
            e.score <= prev + 1e-9,
            "entries not sorted: {:.6} after {:.6}",
            e.score,
            prev
        );
        prev = e.score;
    }

    // We should see at least one entry from `jarvis-core`.
    let has_core = map.entries.iter().any(|e| {
        e.file
            .to_string_lossy()
            .replace('\\', "/")
            .contains("jarvis-core/")
    });
    assert!(
        has_core,
        "expected at least one entry from jarvis-core, got: {:?}",
        map.entries
            .iter()
            .map(|e| e.file.display().to_string())
            .collect::<Vec<_>>()
    );
}
