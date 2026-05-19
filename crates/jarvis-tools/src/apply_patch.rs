//! Codex-style `apply_patch` envelope: parse + apply.
//!
//! Format (verbatim from `openai/codex` `apply_patch_tool_instructions.md`):
//!
//! ```text
//! *** Begin Patch
//! *** Update File: path/to/file
//! @@ optional anchor (a line that appears in the file)
//!  unchanged context line
//! -removed line
//! +added line
//!  unchanged context line
//! *** End Patch
//! ```
//!
//! Plus `*** Add File: <path>`, `*** Delete File: <path>`, and `*** Move to: <path>`
//! (the latter as a sub-header inside an Update File block).
//!
//! Why this and not unified diffs: the envelope omits line numbers (so the model
//! never has to compute them), tolerates surrounding edits via the `@@ anchor`,
//! and produces tiny output even for changes deep inside large files. On a slow
//! local model this is the single highest-leverage edit primitive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub anchor: Option<String>,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    #[error("missing `*** Begin Patch` header")]
    NoBegin,
    #[error("missing `*** End Patch` footer")]
    NoEnd,
    #[error("unrecognized header line {line}: {content:?}")]
    BadHeader { line: usize, content: String },
    #[error("hunk line outside a file section (line {line})")]
    StrayHunk { line: usize },
    #[error("file `{path}` not found for Update/Delete")]
    FileMissing { path: String },
    #[error("file `{path}` already exists for Add")]
    FileExists { path: String },
    #[error("anchor `{anchor:?}` not found in `{path}`")]
    AnchorMissing { path: String, anchor: String },
    #[error("hunk in `{path}` could not be located in the file")]
    HunkUnmatched { path: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Parse the envelope text into a list of file operations. Whitespace before
/// `*** Begin Patch` and after `*** End Patch` is tolerated.
pub fn parse(input: &str) -> Result<Vec<FileOp>, PatchError> {
    let mut lines = input.lines().enumerate();
    // Find the begin marker.
    let mut started = false;
    for (_, raw) in &mut lines {
        if raw.trim_end() == "*** Begin Patch" {
            started = true;
            break;
        }
    }
    if !started {
        return Err(PatchError::NoBegin);
    }

    let mut ops: Vec<FileOp> = Vec::new();
    let mut state: Option<PendingOp> = None;
    let mut ended = false;

    for (idx, raw) in lines {
        let line = strip_trailing_cr(raw);
        // Headers.
        if let Some(rest) = line.strip_prefix("*** ") {
            // Commit any in-flight op before opening a new section.
            if let Some(pending) = state.take() {
                ops.push(pending.finish());
            }
            if rest == "End Patch" {
                ended = true;
                break;
            } else if let Some(path) = rest.strip_prefix("Update File: ") {
                state = Some(PendingOp::Update {
                    path: path.trim().to_string(),
                    move_to: None,
                    hunks: Vec::new(),
                    current: None,
                });
            } else if let Some(path) = rest.strip_prefix("Add File: ") {
                state = Some(PendingOp::Add {
                    path: path.trim().to_string(),
                    content: String::new(),
                });
            } else if let Some(path) = rest.strip_prefix("Delete File: ") {
                ops.push(FileOp::Delete {
                    path: path.trim().to_string(),
                });
                state = None;
            } else if let Some(path) = rest.strip_prefix("Move to: ") {
                // Move sub-header — attaches to the in-flight Update.
                if let Some(PendingOp::Update { move_to, .. }) = state.as_mut() {
                    *move_to = Some(path.trim().to_string());
                } else {
                    return Err(PatchError::BadHeader {
                        line: idx + 1,
                        content: line.to_string(),
                    });
                }
            } else {
                return Err(PatchError::BadHeader {
                    line: idx + 1,
                    content: line.to_string(),
                });
            }
            continue;
        }

        match state.as_mut() {
            Some(PendingOp::Add { content, .. }) => {
                // Add File: every body line starts with `+` (Codex convention) — we
                // strip the leading `+` if present, otherwise take the line as-is.
                let body = line.strip_prefix('+').unwrap_or(line);
                content.push_str(body);
                content.push('\n');
            }
            Some(PendingOp::Update { hunks, current, .. }) => {
                if let Some(anchor) = line.strip_prefix("@@") {
                    // Close any open hunk and start a new one with this anchor.
                    if let Some(h) = current.take() {
                        hunks.push(h);
                    }
                    *current = Some(Hunk {
                        anchor: Some(anchor.trim().to_string()),
                        lines: Vec::new(),
                    });
                    continue;
                }
                let hunk = current.get_or_insert_with(|| Hunk {
                    anchor: None,
                    lines: Vec::new(),
                });
                if let Some(rest) = line.strip_prefix('+') {
                    hunk.lines.push(HunkLine::Add(rest.to_string()));
                } else if let Some(rest) = line.strip_prefix('-') {
                    hunk.lines.push(HunkLine::Remove(rest.to_string()));
                } else if let Some(rest) = line.strip_prefix(' ') {
                    hunk.lines.push(HunkLine::Context(rest.to_string()));
                } else if line.is_empty() {
                    // Bare empty line counts as blank context.
                    hunk.lines.push(HunkLine::Context(String::new()));
                } else {
                    // Tolerant fallback: an Update-section body line without a
                    // recognized prefix (e.g. `>` blockquote, `#` heading, code)
                    // is taken as a CONTEXT line. The model often forgets the
                    // leading space on lines that already start with markdown
                    // punctuation. The hunk matcher still validates the line
                    // against the real file, so a true mismatch fails loudly
                    // with `HunkUnmatched` rather than silently corrupting.
                    hunk.lines.push(HunkLine::Context(line.to_string()));
                }
            }
            None => {
                if line.trim().is_empty() {
                    continue;
                }
                return Err(PatchError::StrayHunk { line: idx + 1 });
            }
        }
    }

    if let Some(pending) = state.take() {
        ops.push(pending.finish());
    }
    if !ended {
        return Err(PatchError::NoEnd);
    }
    Ok(ops)
}

enum PendingOp {
    Add {
        path: String,
        content: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        hunks: Vec<Hunk>,
        current: Option<Hunk>,
    },
}

impl PendingOp {
    fn finish(self) -> FileOp {
        match self {
            PendingOp::Add { path, content } => FileOp::Add { path, content },
            PendingOp::Update {
                path,
                move_to,
                mut hunks,
                current,
            } => {
                if let Some(h) = current {
                    hunks.push(h);
                }
                FileOp::Update {
                    path,
                    move_to,
                    hunks,
                }
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct ApplyReport {
    pub files: Vec<FileChange>,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub status: ChangeStatus,
    pub lines_added: usize,
    pub lines_removed: usize,
    /// Unified-style diff with leading `+`/`-`/` ` per line, suitable for the
    /// same renderer the web UI uses for `fs_write`. Capped at MAX_DIFF_LINES.
    pub diff_unified: String,
    pub diff_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    Moved,
}

impl ChangeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Moved => "moved",
        }
    }
}

/// Apply the parsed patch to the filesystem rooted at `workdir`. Each `path` in
/// the patch is resolved against `workdir` via `resolve_inside` — paths that
/// escape are rejected. All operations apply or none (best-effort: we write
/// file-by-file but rolling back is not attempted).
pub fn apply(ops: &[FileOp], workdir: &Path) -> Result<ApplyReport, PatchError> {
    let mut report = ApplyReport::default();
    // De-dup: if the same file is touched multiple times we want the report to
    // collapse the count, preserving the first status.
    let mut idx: BTreeMap<String, usize> = BTreeMap::new();

    for op in ops {
        match op {
            FileOp::Delete { path } => {
                let p = resolve_inside(workdir, path)?;
                let existing = std::fs::read_to_string(&p)
                    .map_err(|_| PatchError::FileMissing { path: path.clone() })?;
                std::fs::remove_file(&p)?;
                let removed = existing.lines().count();
                let (diff, truncated) = build_diff(&existing, "");
                bump(
                    &mut report,
                    &mut idx,
                    path.clone(),
                    ChangeStatus::Deleted,
                    0,
                    removed,
                    diff,
                    truncated,
                );
            }
            FileOp::Add { path, content } => {
                let p = resolve_inside(workdir, path)?;
                if p.exists() {
                    return Err(PatchError::FileExists { path: path.clone() });
                }
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&p, content)?;
                let added = content.lines().count();
                let (diff, truncated) = build_diff("", content);
                bump(
                    &mut report,
                    &mut idx,
                    path.clone(),
                    ChangeStatus::Added,
                    added,
                    0,
                    diff,
                    truncated,
                );
            }
            FileOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let p = resolve_inside(workdir, path)?;
                let existing = std::fs::read_to_string(&p)
                    .map_err(|_| PatchError::FileMissing { path: path.clone() })?;
                let (new_content, adds, removes) = apply_hunks(path, &existing, hunks)?;
                let (diff, truncated) = build_diff(&existing, &new_content);
                let dest = match move_to {
                    Some(m) => resolve_inside(workdir, m)?,
                    None => p.clone(),
                };
                if dest != p {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&dest, &new_content)?;
                    std::fs::remove_file(&p)?;
                    bump(
                        &mut report,
                        &mut idx,
                        move_to.clone().unwrap_or_else(|| path.clone()),
                        ChangeStatus::Moved,
                        adds,
                        removes,
                        diff,
                        truncated,
                    );
                } else {
                    std::fs::write(&p, &new_content)?;
                    bump(
                        &mut report,
                        &mut idx,
                        path.clone(),
                        ChangeStatus::Modified,
                        adds,
                        removes,
                        diff,
                        truncated,
                    );
                }
            }
        }
    }
    Ok(report)
}

const MAX_DIFF_LINES: usize = 400;

fn build_diff(old: &str, new: &str) -> (String, bool) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    let mut truncated = false;
    for (emitted, change) in diff.iter_all_changes().enumerate() {
        if emitted >= MAX_DIFF_LINES {
            truncated = true;
            out.push_str("…[truncated]\n");
            break;
        }
        let sign = match change.tag() {
            ChangeTag::Insert => "+",
            ChangeTag::Delete => "-",
            ChangeTag::Equal => " ",
        };
        out.push_str(sign);
        let s = change.as_str().unwrap_or("");
        out.push_str(s);
        if !s.ends_with('\n') {
            out.push('\n');
        }
    }
    (out, truncated)
}

#[allow(clippy::too_many_arguments)]
fn bump(
    report: &mut ApplyReport,
    idx: &mut BTreeMap<String, usize>,
    path: String,
    status: ChangeStatus,
    add: usize,
    rem: usize,
    diff: String,
    truncated: bool,
) {
    match idx.get(&path) {
        Some(&i) => {
            let f = &mut report.files[i];
            f.lines_added += add;
            f.lines_removed += rem;
            // Concatenate diffs from successive hunks on the same path.
            if !diff.is_empty() {
                f.diff_unified.push_str(&diff);
                f.diff_truncated = f.diff_truncated || truncated;
            }
        }
        None => {
            idx.insert(path.clone(), report.files.len());
            report.files.push(FileChange {
                path,
                status,
                lines_added: add,
                lines_removed: rem,
                diff_unified: diff,
                diff_truncated: truncated,
            });
        }
    }
}

fn apply_hunks(
    path: &str,
    existing: &str,
    hunks: &[Hunk],
) -> Result<(String, usize, usize), PatchError> {
    let original_lines: Vec<&str> = existing.lines().collect();
    let mut output: Vec<String> = Vec::with_capacity(original_lines.len());
    let mut cursor = 0usize;
    let mut total_add = 0usize;
    let mut total_rem = 0usize;

    for hunk in hunks {
        // If anchor present, snap forward to the first matching line.
        if let Some(anchor) = &hunk.anchor {
            let start_search = cursor;
            let found = (start_search..original_lines.len())
                .find(|i| original_lines[*i].contains(anchor.as_str()));
            match found {
                Some(pos) => {
                    while cursor < pos {
                        output.push(original_lines[cursor].to_string());
                        cursor += 1;
                    }
                }
                None => {
                    return Err(PatchError::AnchorMissing {
                        path: path.to_string(),
                        anchor: anchor.clone(),
                    });
                }
            }
        }

        // Build the "expected" old segment from Context + Remove lines (in order)
        // and the replacement from Context + Add. Find the expected segment as a
        // contiguous slice starting at `cursor`; allow a small forward window if
        // the model placed extra blanks before the hunk.
        let expected: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect();

        let max_drift = 32; // be tolerant if the hunk starts a few blank lines off
        let mut matched_at: Option<usize> = None;
        for start in cursor
            ..original_lines
                .len()
                .saturating_sub(expected.len())
                .min(cursor + max_drift)
                + 1
        {
            if start + expected.len() > original_lines.len() {
                break;
            }
            if (0..expected.len()).all(|i| original_lines[start + i] == expected[i]) {
                matched_at = Some(start);
                break;
            }
        }
        let match_start = matched_at.ok_or_else(|| PatchError::HunkUnmatched {
            path: path.to_string(),
        })?;
        // Flush untouched lines between cursor and match_start.
        while cursor < match_start {
            output.push(original_lines[cursor].to_string());
            cursor += 1;
        }

        // Emit the replacement: Context + Add (in the order the patch lists them).
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(s) => output.push(s.clone()),
                HunkLine::Add(s) => {
                    output.push(s.clone());
                    total_add += 1;
                }
                HunkLine::Remove(_) => {
                    total_rem += 1;
                }
            }
        }
        // Advance the cursor past the matched old segment.
        cursor = match_start + expected.len();
    }

    // Flush trailing original lines.
    while cursor < original_lines.len() {
        output.push(original_lines[cursor].to_string());
        cursor += 1;
    }

    let mut joined = output.join("\n");
    // Preserve trailing newline if the original had one.
    if existing.ends_with('\n') {
        joined.push('\n');
    }
    Ok((joined, total_add, total_rem))
}

fn resolve_inside(workdir: &Path, rel: &str) -> Result<PathBuf, PatchError> {
    let p = Path::new(rel);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workdir.join(p)
    };
    // Lexical confinement — same rule as ToolCtx::resolve.
    let mut depth: i32 = 0;
    for comp in joined.strip_prefix(workdir).unwrap_or(&joined).components() {
        match comp {
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::Normal(_) => depth += 1,
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                if !joined.starts_with(workdir) {
                    return Err(PatchError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("path escapes workdir: {rel}"),
                    )));
                }
            }
        }
        if depth < 0 {
            return Err(PatchError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("path escapes workdir: {rel}"),
            )));
        }
    }
    Ok(joined)
}

fn strip_trailing_cr(s: &str) -> &str {
    s.strip_suffix('\r').unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_update() {
        // NOTE: don't use Rust's `\` line-continuation here — it eats leading
        // whitespace, which is exactly what context-line prefixes encode.
        let src = concat!(
            "*** Begin Patch\n",
            "*** Update File: src/foo.rs\n",
            "@@ fn main\n",
            " let x = 1;\n",
            "-    println!(\"hi\");\n",
            "+    println!(\"hello\");\n",
            " let y = 2;\n",
            "*** End Patch\n",
        );
        let ops = parse(src).unwrap();
        assert_eq!(ops.len(), 1);
        match &ops[0] {
            FileOp::Update { path, hunks, .. } => {
                assert_eq!(path, "src/foo.rs");
                assert_eq!(hunks.len(), 1);
                assert_eq!(hunks[0].anchor.as_deref(), Some("fn main"));
                assert_eq!(hunks[0].lines.len(), 4);
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn parses_add_and_delete() {
        let src = concat!(
            "*** Begin Patch\n",
            "*** Add File: hello.txt\n",
            "+line one\n",
            "+line two\n",
            "*** Delete File: bye.txt\n",
            "*** End Patch\n",
        );
        let ops = parse(src).unwrap();
        assert_eq!(ops.len(), 2);
        match &ops[0] {
            FileOp::Add { path, content } => {
                assert_eq!(path, "hello.txt");
                assert_eq!(content, "line one\nline two\n");
            }
            _ => panic!("expected Add"),
        }
        match &ops[1] {
            FileOp::Delete { path } => assert_eq!(path, "bye.txt"),
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn rejects_missing_begin() {
        let err = parse("*** Update File: x\n*** End Patch\n").unwrap_err();
        matches!(err, PatchError::NoBegin);
    }

    #[test]
    fn rejects_missing_end() {
        let err = parse("*** Begin Patch\n*** Update File: x\n").unwrap_err();
        matches!(err, PatchError::NoEnd);
    }

    #[test]
    fn applies_update_round_trip() {
        let tmp = tempdir();
        let path = tmp.path().join("foo.rs");
        std::fs::write(
            &path,
            "fn main() {\n    println!(\"hi\");\n    let y = 2;\n}\n",
        )
        .unwrap();
        let src = concat!(
            "*** Begin Patch\n",
            "*** Update File: foo.rs\n",
            "@@ fn main\n",
            " fn main() {\n",
            "-    println!(\"hi\");\n",
            "+    println!(\"hello\");\n",
            "     let y = 2;\n",
            "*** End Patch\n",
        );
        let ops = parse(src).unwrap();
        let r = apply(&ops, tmp.path()).unwrap();
        assert_eq!(r.files.len(), 1);
        assert_eq!(r.files[0].lines_added, 1);
        assert_eq!(r.files[0].lines_removed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("println!(\"hello\");"));
        assert!(!after.contains("println!(\"hi\");"));
    }

    #[test]
    fn tolerates_unprefixed_context_lines() {
        // Real bug seen against README.md: the agent omitted the leading space
        // on a `> blockquote` line, expecting it to be context. Strict mode
        // rejected the patch; tolerant mode treats it as context and the hunk
        // matcher validates against the actual file.
        let tmp = tempdir();
        let path = tmp.path().join("README.md");
        std::fs::write(&path, "# Jarvis\n\n> blockquote line.\n").unwrap();
        let src = concat!(
            "*** Begin Patch\n",
            "*** Update File: README.md\n",
            "@@ \n",
            "+// hello\n",
            " # Jarvis\n",
            "\n",
            "> blockquote line.\n",
            "*** End Patch\n",
        );
        let ops = parse(src).expect("parser should tolerate unprefixed lines");
        let r = apply(&ops, tmp.path()).expect("apply should match against the real file");
        assert_eq!(r.files.len(), 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with("// hello\n"));
        assert!(after.contains("# Jarvis"));
        assert!(after.contains("> blockquote line."));
    }

    #[test]
    fn applies_add_and_delete() {
        let tmp = tempdir();
        let to_delete = tmp.path().join("old.txt");
        std::fs::write(&to_delete, "bye\n").unwrap();
        let src = concat!(
            "*** Begin Patch\n",
            "*** Add File: new.txt\n",
            "+hello\n",
            "+world\n",
            "*** Delete File: old.txt\n",
            "*** End Patch\n",
        );
        let ops = parse(src).unwrap();
        apply(&ops, tmp.path()).unwrap();
        let new = std::fs::read_to_string(tmp.path().join("new.txt")).unwrap();
        assert_eq!(new, "hello\nworld\n");
        assert!(!tmp.path().join("old.txt").exists());
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }
}
