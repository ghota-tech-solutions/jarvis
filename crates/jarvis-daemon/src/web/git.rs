//! Git endpoints + helpers + rendering. Mirrors what the right-sidebar git
//! panel surfaces: commit, reset, diff preview, stage/unstage, PR-url shortcut,
//! and the LLM-driven "suggest commit message" path.
//!
//! Everything here is workdir-scoped and synchronous against libgit2; the
//! axum handlers wrap the blocking calls in `spawn_blocking` so they never
//! park the runtime.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::{Form, Json};
use maud::{html, Markup, PreEscaped};
use serde::{Deserialize, Serialize};

use super::util::{format_age, json_string_literal, AppError};
use super::WebState;

// ----------------- Form / Query DTOs -----------------

#[derive(Deserialize)]
pub(super) struct GitCommitForm {
    workdir: String,
    message: String,
    /// "on" when the modal's "stage all changes" checkbox is ticked.
    #[serde(default)]
    stage_all: String,
}

#[derive(Deserialize)]
pub(super) struct PrUrlQuery {
    workdir: String,
}

#[derive(Clone, Copy)]
enum DiffScope {
    /// `git diff` — working tree vs index.
    Unstaged,
    /// `git diff --staged` — index vs HEAD.
    Staged,
    /// Working tree + index vs HEAD (what would land in a commit-all).
    All,
}

#[derive(Deserialize)]
pub(super) struct DiffQuery {
    workdir: String,
    /// Relative path within the repo; if empty, diff the whole tree.
    #[serde(default)]
    path: String,
    /// `unstaged` (default), `staged`, or `all`.
    #[serde(default)]
    scope: String,
}

#[derive(Deserialize)]
pub(super) struct StageForm {
    workdir: String,
    /// Comma-separated list of paths (one path per checkbox).
    paths: String,
}

#[derive(Deserialize)]
pub(super) struct ResetForm {
    workdir: String,
    /// `mixed` (default), `soft`, or `hard`. `hard` is destructive — UI must confirm.
    #[serde(default)]
    mode: String,
    /// Defaults to `HEAD`.
    #[serde(default)]
    target: String,
}

#[derive(Deserialize)]
pub(super) struct SuggestQuery {
    workdir: String,
    /// `staged` (default) or `all`.
    #[serde(default)]
    scope: String,
}

#[derive(Deserialize)]
pub(super) struct GitQuery {
    workdir: String,
}

// ----------------- GitInfo (the panel's data model) -----------------

#[derive(Serialize)]
pub(super) struct GitInfo {
    branch: String,
    changes_added: usize,
    changes_removed: usize,
    files: Vec<GitFileChange>,
    /// Most-recent commits on HEAD (oldest at the end of the vec).
    commits: Vec<GitCommit>,
    /// Upstream tracking branch + ahead/behind, when configured.
    upstream: Option<UpstreamInfo>,
}

#[derive(Serialize)]
struct GitFileChange {
    path: String,
    /// "modified" | "new" | "deleted" | "renamed"
    status: String,
}

#[derive(Serialize)]
struct GitCommit {
    hash: String,
    subject: String,
    author: String,
    age_seconds: i64,
}

#[derive(Serialize)]
struct UpstreamInfo {
    #[allow(dead_code)] // serialized for JSON consumers; UI renders ahead/behind only
    name: String,
    ahead: usize,
    behind: usize,
}

// ----------------- Axum handlers -----------------

pub(super) async fn api_git_commit(
    headers: axum::http::HeaderMap,
    Form(form): Form<GitCommitForm>,
) -> Result<Response, AppError> {
    if form.message.trim().is_empty() {
        return Err(AppError::BadRequest("commit message is empty".into()));
    }
    let stage_all = matches!(form.stage_all.as_str(), "on" | "true" | "1");
    let workdir = form.workdir.clone();
    let message = form.message.clone();
    let oid = match tokio::task::spawn_blocking(move || git_commit(&workdir, &message, stage_all))
        .await
    {
        Ok(Ok(oid)) => oid,
        Ok(Err(e)) => return Err(AppError::Internal(format!("git commit: {e}"))),
        Err(e) => return Err(AppError::Internal(format!("join: {e}"))),
    };
    let is_htmx = headers
        .get("HX-Request")
        .map(|v| v.to_str().unwrap_or("").eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if is_htmx {
        Ok((
            StatusCode::OK,
            [("HX-Trigger", "jarvis-git-committed")],
            format!("<div class=\"muted small\">committed {}</div>", &oid[..7]),
        )
            .into_response())
    } else {
        Ok(Json(serde_json::json!({ "ok": true, "oid": oid })).into_response())
    }
}

pub(super) async fn api_git_pr_url(Query(q): Query<PrUrlQuery>) -> Response {
    let workdir = q.workdir.clone();
    let info = tokio::task::spawn_blocking(move || git_pr_url(&workdir)).await;
    match info {
        Ok(Some(url)) => Json(serde_json::json!({ "url": url })).into_response(),
        _ => Json(serde_json::json!({ "url": null })).into_response(),
    }
}

pub(super) async fn api_git_diff(Query(q): Query<DiffQuery>) -> Response {
    let scope = match q.scope.as_str() {
        "staged" => DiffScope::Staged,
        "all" => DiffScope::All,
        _ => DiffScope::Unstaged,
    };
    let workdir = q.workdir.clone();
    let path = if q.path.is_empty() {
        None
    } else {
        Some(q.path.clone())
    };
    let diff = tokio::task::spawn_blocking(move || git_unified_diff(&workdir, scope, path.as_deref()))
        .await
        .ok()
        .flatten();
    match diff {
        Some(text) if !text.is_empty() => Html(
            html! {
                div class="diff-card" {
                    div class="diff-card-head" {
                        strong { (if q.path.is_empty() { "Working tree diff" } else { q.path.as_str() }) }
                    }
                    pre class="diff-body" {
                        (PreEscaped(super::render::colorize_unified(&text)))
                    }
                }
            }
            .into_string(),
        )
        .into_response(),
        _ => Html(r#"<div class="muted small">no changes</div>"#.to_string()).into_response(),
    }
}

pub(super) async fn api_git_stage(Form(form): Form<StageForm>) -> Response {
    git_stage_paths(&form.workdir, &form.paths, true)
}

pub(super) async fn api_git_unstage(Form(form): Form<StageForm>) -> Response {
    git_stage_paths(&form.workdir, &form.paths, false)
}

pub(super) async fn api_git_reset(Form(form): Form<ResetForm>) -> Result<Response, AppError> {
    let workdir = form.workdir.clone();
    let mode_str = if form.mode.is_empty() {
        "mixed".to_string()
    } else {
        form.mode.clone()
    };
    let target = if form.target.is_empty() {
        "HEAD".to_string()
    } else {
        form.target.clone()
    };
    let r = tokio::task::spawn_blocking(move || git_reset(&workdir, &mode_str, &target))
        .await
        .map_err(|e| AppError::Internal(format!("join: {e}")))?;
    match r {
        Ok(_) => Ok((
            StatusCode::OK,
            [("HX-Trigger", "jarvis-git-committed")],
            r#"<span class="muted small">reset ok</span>"#.to_string(),
        )
            .into_response()),
        Err(e) => Err(AppError::Internal(format!("reset: {e}"))),
    }
}

pub(super) async fn api_git_suggest_message(
    State(s): State<WebState>,
    Query(q): Query<SuggestQuery>,
) -> Result<Response, AppError> {
    let scope = match q.scope.as_str() {
        "all" => DiffScope::All,
        _ => DiffScope::Staged,
    };
    let workdir = q.workdir.clone();
    let mut diff = tokio::task::spawn_blocking(move || git_unified_diff(&workdir, scope, None))
        .await
        .map_err(|e| AppError::Internal(format!("join: {e}")))?
        .ok_or_else(|| AppError::BadRequest("not a git repo".into()))?;
    if diff.trim().is_empty() {
        // Fall back to the full working-tree diff when nothing is staged.
        let wd = q.workdir.clone();
        diff = tokio::task::spawn_blocking(move || git_unified_diff(&wd, DiffScope::All, None))
            .await
            .map_err(|e| AppError::Internal(format!("join: {e}")))?
            .unwrap_or_default();
    }
    if diff.trim().is_empty() {
        return Err(AppError::BadRequest("no changes to summarize".into()));
    }
    // Cap diff size to keep the local model's context light.
    const MAX_DIFF_CHARS: usize = 8000;
    if diff.len() > MAX_DIFF_CHARS {
        diff.truncate(MAX_DIFF_CHARS);
        diff.push_str("\n…(diff truncated)…\n");
    }

    use jarvis_core::{ChatMessage, ChatRequest, RequiredCapabilities, TaskKind};
    use jarvis_llm::PickRequest;
    let req = PickRequest {
        required: RequiredCapabilities::default(),
        kind: TaskKind::Summarize,
        estimated_tokens: 0,
        routing_override: None,
        forbidden: Default::default(),
    };
    let picked = s
        .pool
        .pick(&req)
        .await
        .map_err(|e| AppError::Internal(format!("pick: {e}")))?;
    let messages = vec![
        ChatMessage::system(
            "You write concise git commit messages.\n\
             Output ONLY the commit message — no preamble, no markdown code fences, no quotes.\n\
             Format: subject ≤ 72 chars, imperative mood, no trailing period.\n\
             Optional body: blank line, then body wrapped at 72 chars.\n\
             Prefer Conventional-Commits prefixes when obvious (feat, fix, refactor, docs, test, chore).",
        ),
        ChatMessage::user(format!(
            "Diff:\n\n```\n{diff}\n```\n\nWrite the commit message."
        )),
    ];
    let chat = ChatRequest {
        messages,
        temperature: Some(0.2),
        max_tokens: Some(200),
        stream: false,
    };
    let resp = picked
        .provider
        .complete(chat)
        .await
        .map_err(|e| AppError::Internal(format!("llm: {e}")))?;
    let msg = sanitize_commit_message(&resp.content);
    Ok(Json(serde_json::json!({ "message": msg, "model": picked.name.as_str() })).into_response())
}

pub(super) async fn api_git(Query(q): Query<GitQuery>) -> Response {
    let workdir = q.workdir.clone();
    let info = tokio::task::spawn_blocking({
        let w = workdir.clone();
        move || git_status(&w)
    })
    .await;
    match info {
        Ok(Some(info)) => Html(render_git_card(&info, &workdir).into_string()).into_response(),
        _ => Html(render_git_empty().into_string()).into_response(),
    }
}

// ----------------- Pure git operations -----------------

fn git_commit(workdir: &str, message: &str, stage_all: bool) -> Result<String, String> {
    use git2::{IndexAddOption, Repository};
    let repo = Repository::discover(workdir).map_err(|e| format!("discover: {e}"))?;
    let sig = repo
        .signature()
        .map_err(|e| format!("signature (set user.name/user.email): {e}"))?;
    let mut index = repo.index().map_err(|e| format!("index: {e}"))?;
    if stage_all {
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .map_err(|e| format!("add_all: {e}"))?;
        index.write().map_err(|e| format!("index.write: {e}"))?;
    }
    let tree_id = index.write_tree().map_err(|e| format!("write_tree: {e}"))?;
    let tree = repo.find_tree(tree_id).map_err(|e| format!("find_tree: {e}"))?;
    let parent_commit = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    if !stage_all && parent_commit.is_some() {
        // Refuse an empty commit (nothing in index changed since HEAD).
        let parent_tree = parent_commit.as_ref().and_then(|c| c.tree().ok());
        if let Some(pt) = parent_tree
            && pt.id() == tree_id
        {
            return Err("nothing staged".to_string());
        }
    }
    let parents: Vec<&git2::Commit> = parent_commit.as_ref().map(|c| vec![c]).unwrap_or_default();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(|e| format!("commit: {e}"))?;
    Ok(oid.to_string())
}

fn git_pr_url(workdir: &str) -> Option<String> {
    use git2::Repository;
    let repo = Repository::discover(workdir).ok()?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(|s| s.to_string()))?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?.to_string();
    let (owner, name) = parse_github_url(&url)?;
    Some(format!(
        "https://github.com/{owner}/{name}/compare/{branch}?expand=1"
    ))
}

fn git_unified_diff(workdir: &str, scope: DiffScope, path: Option<&str>) -> Option<String> {
    use git2::{DiffFormat, DiffOptions, Repository};
    let repo = Repository::discover(workdir).ok()?;
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut opts = DiffOptions::new();
    opts.context_lines(3).interhunk_lines(2).ignore_submodules(true);
    if let Some(p) = path {
        opts.pathspec(p);
    }
    let diff = match scope {
        DiffScope::Unstaged => repo.diff_index_to_workdir(None, Some(&mut opts)).ok()?,
        DiffScope::Staged => repo
            .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))
            .ok()?,
        DiffScope::All => repo
            .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))
            .ok()?,
    };
    let mut out = String::new();
    diff.print(DiffFormat::Patch, |_, _, line| {
        let origin = line.origin();
        if matches!(origin, '+' | '-' | ' ') {
            out.push(origin);
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .ok()?;
    Some(out)
}

fn git_stage_paths(workdir: &str, paths_csv: &str, stage: bool) -> Response {
    use git2::{IndexAddOption, Repository};
    let paths: Vec<&str> = paths_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if paths.is_empty() {
        return AppError::BadRequest("no paths".into()).into_response();
    }
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(e) => return AppError::Internal(format!("discover: {e}")).into_response(),
    };
    let res: Result<(), git2::Error> = if stage {
        let mut index = match repo.index() {
            Ok(i) => i,
            Err(e) => return AppError::Internal(format!("index: {e}")).into_response(),
        };
        index
            .add_all(paths.iter(), IndexAddOption::DEFAULT, None)
            .and_then(|_| index.write())
    } else {
        // Unstage: reset_default rewrites the index entries for these paths
        // back to their HEAD state. On an unborn HEAD, just remove them from
        // the index.
        match repo
            .head()
            .ok()
            .and_then(|h| h.peel(git2::ObjectType::Commit).ok())
        {
            Some(commit) => repo.reset_default(Some(&commit), paths.iter()),
            None => {
                let mut index = match repo.index() {
                    Ok(i) => i,
                    Err(e) => return AppError::Internal(format!("index: {e}")).into_response(),
                };
                index
                    .remove_all(paths.iter(), None)
                    .and_then(|_| index.write())
            }
        }
    };
    match res {
        Ok(_) => (
            StatusCode::OK,
            [("HX-Trigger", "jarvis-git-committed")],
            r#"<span class="muted small">ok</span>"#.to_string(),
        )
            .into_response(),
        Err(e) => AppError::Internal(format!("stage: {e}")).into_response(),
    }
}

fn git_reset(workdir: &str, mode: &str, target: &str) -> Result<(), String> {
    use git2::{Repository, ResetType};
    let repo = Repository::discover(workdir).map_err(|e| format!("discover: {e}"))?;
    let obj = repo
        .revparse_single(target)
        .map_err(|e| format!("revparse {target}: {e}"))?;
    let kind = match mode {
        "soft" => ResetType::Soft,
        "hard" => ResetType::Hard,
        _ => ResetType::Mixed,
    };
    repo.reset(&obj, kind, None)
        .map_err(|e| format!("reset: {e}"))
}

/// Strip stray code fences / quotes / "Subject:" prefixes the model sometimes adds.
pub(super) fn sanitize_commit_message(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    // Drop leading/trailing triple-backtick fences.
    if let Some(rest) = s.strip_prefix("```") {
        s = match rest.find('\n') {
            Some(idx) => rest[idx + 1..].to_string(),
            None => rest.to_string(),
        };
    }
    if let Some(rest) = s.strip_suffix("```") {
        s = rest.trim_end().to_string();
    }
    // Drop a leading "Subject: " label if present.
    if let Some(rest) = s
        .strip_prefix("Subject:")
        .or_else(|| s.strip_prefix("subject:"))
    {
        s = rest.trim_start().to_string();
    }
    // Drop wrapping quotes.
    let trimmed = s.trim();
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 1 {
        s = trimmed[1..trimmed.len() - 1].to_string();
    }
    s.trim().to_string()
}

/// Accepts both `https://github.com/owner/repo(.git)` and `git@github.com:owner/repo(.git)` forms.
pub(super) fn parse_github_url(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let after_host = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"))?;
    let (owner, name) = after_host.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

fn git_status(workdir: &str) -> Option<GitInfo> {
    use git2::{BranchType, Repository, Sort, Status, StatusOptions};
    let repo = Repository::discover(workdir).ok()?;
    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(|s| s.to_string()))
        .unwrap_or_else(|| "(detached)".to_string());

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(false);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;

    let mut files: Vec<GitFileChange> = Vec::new();
    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("").to_string();
        let s = entry.status();
        let status = if s.intersects(Status::WT_NEW | Status::INDEX_NEW) {
            "new"
        } else if s.intersects(Status::WT_DELETED | Status::INDEX_DELETED) {
            "deleted"
        } else if s.intersects(Status::WT_RENAMED | Status::INDEX_RENAMED) {
            "renamed"
        } else if s.intersects(
            Status::WT_MODIFIED
                | Status::INDEX_MODIFIED
                | Status::WT_TYPECHANGE
                | Status::INDEX_TYPECHANGE,
        ) {
            "modified"
        } else {
            continue;
        };
        files.push(GitFileChange {
            path,
            status: status.to_string(),
        });
    }

    // Per-file diff stats via `git diff --numstat` semantics; we use libgit2.
    let mut added_total = 0usize;
    let mut removed_total = 0usize;
    if let Ok(head) = repo.head().and_then(|h| h.peel_to_tree()) {
        let mut diff_opts = git2::DiffOptions::new();
        diff_opts
            .include_untracked(true)
            .recurse_untracked_dirs(false);
        if let Ok(diff) = repo.diff_tree_to_workdir_with_index(Some(&head), Some(&mut diff_opts))
            && let Ok(stats) = diff.stats()
        {
            added_total = stats.insertions();
            removed_total = stats.deletions();
        }
    }

    // Most-recent commits on HEAD.
    let now_secs = chrono::Utc::now().timestamp();
    let mut commits: Vec<GitCommit> = Vec::new();
    if let Ok(mut walk) = repo.revwalk() {
        let _ = walk.set_sorting(Sort::TIME | Sort::TOPOLOGICAL);
        if walk.push_head().is_ok() {
            for oid in walk.take(8).flatten() {
                if let Ok(commit) = repo.find_commit(oid) {
                    let hash: String = oid.to_string().chars().take(7).collect();
                    let subject = commit
                        .summary()
                        .unwrap_or("(no message)")
                        .chars()
                        .take(120)
                        .collect::<String>();
                    let author = commit.author().name().unwrap_or("").to_string();
                    let age_seconds = (now_secs - commit.time().seconds()).max(0);
                    commits.push(GitCommit {
                        hash,
                        subject,
                        author,
                        age_seconds,
                    });
                }
            }
        }
    }

    // Upstream tracking (ahead/behind).
    let upstream = (|| -> Option<UpstreamInfo> {
        let local = repo.find_branch(&branch, BranchType::Local).ok()?;
        let upstream_branch = local.upstream().ok()?;
        let upstream_name = upstream_branch
            .name()
            .ok()
            .flatten()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let local_oid = local.get().target()?;
        let upstream_oid = upstream_branch.get().target()?;
        let (ahead, behind) = repo.graph_ahead_behind(local_oid, upstream_oid).ok()?;
        Some(UpstreamInfo {
            name: upstream_name,
            ahead,
            behind,
        })
    })();

    Some(GitInfo {
        branch,
        changes_added: added_total,
        changes_removed: removed_total,
        files,
        commits,
        upstream,
    })
}

// ----------------- Rendering -----------------

fn render_git_empty() -> Markup {
    html! {
        div class="git-empty" {
            div class="glyph" { "⎇" }
            div class="git-empty-title" { "Not under version control" }
            div class="muted small" { "This workdir is not in a git repository." }
        }
    }
}

fn render_git_card(info: &GitInfo, workdir: &str) -> Markup {
    let has_changes =
        info.changes_added + info.changes_removed > 0 || !info.files.is_empty();
    let has_upstream = info.upstream.is_some();
    let workdir_json = json_string_literal(workdir);

    html! {
        // Action row: Commit / Unstage / Discard / PR. Hidden when nothing is actionable.
        @if has_changes || has_upstream {
            div class="git-actions" data-workdir=(workdir) {
                @if has_changes {
                    button type="button" class="git-btn"
                           onclick={ "document.getElementById('git-commit-modal').classList.add('open'); document.getElementById('git-commit-workdir').value=" (PreEscaped(workdir_json.as_str())) "; document.getElementById('git-commit-message').focus();" }
                           { "Commit…" }
                    button type="button" class="git-btn ghost"
                           title="git reset HEAD — unstage everything, keep working tree"
                           onclick={ "jarvisGitReset(" (PreEscaped(workdir_json.as_str())) ", 'mixed')" }
                           { "Unstage" }
                    button type="button" class="git-btn danger"
                           title="git reset --hard HEAD — DISCARD all uncommitted changes"
                           onclick={ "jarvisGitReset(" (PreEscaped(workdir_json.as_str())) ", 'hard')" }
                           { "Discard all" }
                }
                @if has_upstream {
                    button type="button" class="git-btn ghost"
                           onclick={ "jarvisOpenPr(" (PreEscaped(workdir_json.as_str())) ")" }
                           { "Create PR" }
                }
            }
        }

        // Branch + ahead/behind row, then totals.
        div class="git-section" {
            div class="git-row git-row-head" {
                span class="git-icon" { "⎇" }
                span class="git-branch" { (info.branch) }
                @match &info.upstream {
                    Some(u) if u.ahead + u.behind > 0 => {
                        span class="git-ahead-behind" {
                            @if u.ahead > 0 { span class="git-ahead" { "↑" (u.ahead) } }
                            @if u.behind > 0 { span class="git-behind" { "↓" (u.behind) } }
                        }
                    }
                    Some(_) => { span class="muted small" { "in sync" } }
                    None => {}
                }
            }
            div class="git-row" {
                span class="git-icon" { "✎" }
                span class="git-row-label" { "Changes" }
                @if info.changes_added + info.changes_removed > 0 || !info.files.is_empty() {
                    span class="git-meta" {
                        span class="add" { "+" (info.changes_added) }
                        " "
                        span class="rem" { "-" (info.changes_removed) }
                    }
                } @else {
                    span class="muted small" { "clean" }
                }
            }
        }

        // Files section.
        @if !info.files.is_empty() {
            div class="git-section" {
                div class="git-section-title" { "Files" }
                @for f in info.files.iter().take(16) {
                    @let (tag, cls) = match f.status.as_str() {
                        "new" => ("A", "add"),
                        "deleted" => ("D", "rem"),
                        "modified" => ("M", "mod"),
                        "renamed" => ("R", "mod"),
                        _ => ("?", "mod"),
                    };
                    div class="git-row git-row-file" data-path=(f.path) title="click to preview diff" {
                        span class={ "git-tag " (cls) } { (tag) }
                        span class="git-row-label monoline" { (f.path) }
                    }
                }
                @if info.files.len() > 16 {
                    div class="muted small" style="padding:0.2em 0.4em;" {
                        "… and " (info.files.len() - 16) " more"
                    }
                }
            }
        }

        // Recent commits section.
        @if !info.commits.is_empty() {
            div class="git-section" {
                div class="git-section-title" { "Recent commits" }
                @for c in &info.commits {
                    @let tooltip = format!("{}\n— {} ago, by {}", c.subject, format_age(c.age_seconds), c.author);
                    div class="git-row git-row-commit" title=(tooltip) {
                        span class="git-hash" { (c.hash) }
                        span class="git-row-label" { (c.subject) }
                        span class="git-meta" { (format_age(c.age_seconds)) }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_url_https() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );
        assert_eq!(
            parse_github_url("https://github.com/owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );
    }

    #[test]
    fn parse_github_url_ssh() {
        assert_eq!(
            parse_github_url("git@github.com:owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );
        assert_eq!(
            parse_github_url("ssh://git@github.com/owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );
    }

    #[test]
    fn parse_github_url_rejects_non_github() {
        assert_eq!(parse_github_url("https://gitlab.com/o/r.git"), None);
        assert_eq!(parse_github_url("file:///tmp/repo"), None);
    }

    #[test]
    fn sanitize_commit_message_strips_fences() {
        assert_eq!(
            sanitize_commit_message("```\nfeat: add things\n\nbody\n```"),
            "feat: add things\n\nbody"
        );
        assert_eq!(sanitize_commit_message("```text\nfix: x\n```"), "fix: x");
    }

    #[test]
    fn sanitize_commit_message_strips_subject_label_and_quotes() {
        assert_eq!(
            sanitize_commit_message("Subject: refactor: clean up router\n"),
            "refactor: clean up router"
        );
        assert_eq!(sanitize_commit_message("\"feat: x\""), "feat: x");
    }
}
