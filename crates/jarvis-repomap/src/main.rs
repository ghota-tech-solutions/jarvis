//! `repomap` — small CLI to inspect what [`jarvis_repomap`] would feed
//! to the agent. Not part of any user-facing flow.
//!
//! Usage: `repomap [PATH] [--budget N]`

use std::path::PathBuf;
use std::process::ExitCode;

use jarvis_repomap::{RepoMapOptions, build_repomap};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let root = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    let budget: usize = args
        .iter()
        .position(|a| a == "--budget")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);

    let opts = RepoMapOptions {
        root: PathBuf::from(root),
        token_budget: budget,
        extra_ignore: vec![],
    };

    let map = match build_repomap(&opts) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("repomap: {e}");
            return ExitCode::FAILURE;
        }
    };

    for e in &map.entries {
        println!(
            "{}:{}\t[{:.4}]\t{}",
            e.file.display(),
            e.line,
            e.score,
            e.signature
        );
    }
    eprintln!(
        "\nscanned {} files, {} entries, ~{} tokens (budget {})",
        map.total_files_scanned,
        map.entries.len(),
        map.token_estimate,
        opts.token_budget,
    );
    ExitCode::SUCCESS
}
