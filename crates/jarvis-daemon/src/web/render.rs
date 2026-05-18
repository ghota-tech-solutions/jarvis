//! HTML rendering for ledger events: action cards, file rollups, diff bodies,
//! conversation streams. None of these helpers touch `WebState` — they are
//! pure (events in, String out), so they live cleanly outside `mod.rs`.

use jarvis_core::TaskId;
use jarvis_ledger::{EventRecord, TaskRecord};
use maud::{html, PreEscaped};

use super::util::{html_escape, render_markdown};

/// Render a slice of events as Codex-style blocks. Hides infrastructure noise
/// (heartbeat, attempt). Pairs tool_call+tool_result into single action cards.
/// Decision/verdict become prose blocks. When `pending_tool_call` carries state
/// across SSE yields, callers can pass it in; here we fold per-call.
pub(super) fn render_event_blocks<'a>(
    events: impl IntoIterator<Item = &'a EventRecord>,
) -> String {
    let mut out = String::new();
    let mut pending_tool: Option<&EventRecord> = None;
    for ev in events {
        match ev.kind.to_string().as_str() {
            // Noise: skip entirely. Step boundaries are implied by tool runs.
            "heartbeat" | "attempt" => {}
            "decision" => {
                pending_tool = None;
                if let Some(t) = ev.payload.get("thought").and_then(|v| v.as_str())
                    && !t.trim().is_empty()
                {
                    out.push_str(
                        &html! {
                            div class="turn assistant" data-id=(ev.id.0) {
                                (PreEscaped(render_markdown(t)))
                            }
                        }
                        .into_string(),
                    );
                }
            }
            "tool_call" => {
                pending_tool = Some(ev);
            }
            "tool_result" => {
                let pair_id = pending_tool.map(|c| c.id.0).unwrap_or(ev.id.0);
                out.push_str(&render_action_card(pending_tool, ev, pair_id));
                pending_tool = None;
            }
            "error" => {
                // Legacy error events recorded from failing tool invocations
                // (M6.7 and earlier) have the shape of a tool_result with
                // `is_error: true` rather than a `message`. Route them through
                // the action-card renderer with a red border, the way M6.7-H+
                // does at the source.
                if ev.payload.get("tool").and_then(|v| v.as_str()).is_some()
                    && (ev.payload.get("summary").is_some() || ev.payload.get("data").is_some())
                {
                    let pair_id = pending_tool.map(|c| c.id.0).unwrap_or(ev.id.0);
                    out.push_str(&render_action_card(pending_tool, ev, pair_id));
                    pending_tool = None;
                    continue;
                }
                pending_tool = None;
                let msg = ev
                    .payload
                    .get("message")
                    .or_else(|| ev.payload.get("error"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no message)");
                out.push_str(
                    &html! {
                        div class="turn error" data-id=(ev.id.0) {
                            span class="err" { "✗ " (msg) }
                        }
                    }
                    .into_string(),
                );
            }
            "verdict" => {
                pending_tool = None;
                let v = ev.payload.get("verdict").and_then(|x| x.as_str()).unwrap_or("?");
                let m = ev.payload.get("message").and_then(|x| x.as_str()).unwrap_or("");
                let cls = match v {
                    "pass" => "verdict-pass",
                    "fail" => "verdict-fail",
                    _ => "verdict-other",
                };
                out.push_str(
                    &html! {
                        div class={ "turn verdict " (cls) } data-id=(ev.id.0) {
                            span class={ "badge " (cls) } { (v) }
                            (PreEscaped(render_markdown(m)))
                        }
                    }
                    .into_string(),
                );
            }
            "continuation" => {
                pending_tool = None;
                let attempt = ev.payload.get("attempt").and_then(|v| v.as_i64()).unwrap_or(1);
                let budget = ev.payload.get("budget").and_then(|v| v.as_i64()).unwrap_or(1);
                out.push_str(
                    &html! {
                        div class="turn continuation" data-id=(ev.id.0) {
                            span class="badge audit" { "↻ audit " (attempt) "/" (budget) }
                            span class="muted small" { "forced continuation — verify before stopping" }
                        }
                    }
                    .into_string(),
                );
            }
            _ => {}
        }
    }
    // If a tool_call had no matching tool_result yet (e.g. streaming), surface it alone.
    if let Some(ev) = pending_tool {
        out.push_str(&render_action_card(Some(ev), ev, ev.id.0));
    }
    out
}

/// Group events by task_id and render each task as its own "turn" block:
/// optional `[user] goal` header (skipped for the root), then the rendered
/// events of that task. The root's goal already lives in the page header.
pub(super) fn render_conversation(chain: &[TaskRecord], events: &[EventRecord]) -> String {
    use std::collections::HashMap;
    let mut by_task: HashMap<TaskId, Vec<&EventRecord>> = HashMap::new();
    for ev in events {
        by_task.entry(ev.task_id).or_default().push(ev);
    }
    html! {
        // Chain is root → leaf, so render in chain order to keep chronology.
        @for (idx, task) in chain.iter().enumerate() {
            // The root's goal is the page H1; only children get a user-message bubble.
            @if idx > 0 {
                div class="turn user" {
                    div class="user-bubble" { (PreEscaped(render_markdown(&task.goal))) }
                }
            }
            @if let Some(evs) = by_task.get(&task.id) {
                (PreEscaped(render_event_blocks(evs.iter().copied())))
            }
        }
    }
    .into_string()
}

/// Walk the events, aggregate fs_write into a "Edited N files" rollup card.
pub(super) fn render_files_rollup(events: &[EventRecord]) -> String {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Agg {
        added: u64,
        removed: u64,
        is_new: bool,
        turns: u32,
    }
    let mut by_path: BTreeMap<String, Agg> = BTreeMap::new();
    for ev in events {
        if ev.kind.to_string() != "tool_result" {
            continue;
        }
        let Some(data) = ev.payload.get("data") else { continue };
        let Some(path) = data.get("path").and_then(|v| v.as_str()) else { continue };
        let Some(added) = data.get("lines_added").and_then(|v| v.as_u64()) else { continue };
        let removed = data.get("lines_removed").and_then(|v| v.as_u64()).unwrap_or(0);
        let is_new = data.get("is_new").and_then(|v| v.as_bool()).unwrap_or(false);
        let entry = by_path.entry(path.to_string()).or_default();
        entry.added += added;
        entry.removed += removed;
        entry.is_new = entry.is_new || is_new;
        entry.turns += 1;
    }
    if by_path.is_empty() {
        return String::new();
    }
    let total_added: u64 = by_path.values().map(|a| a.added).sum();
    let total_removed: u64 = by_path.values().map(|a| a.removed).sum();
    let file_count = by_path.len();
    let label = if file_count == 1 { "file" } else { "files" };
    html! {
        div class="rollup card" {
            div class="rollup-head" {
                span class="rollup-title" { "Edited " (file_count) " " (label) }
                span class="add" { "+" (total_added) }
                span class="rem" { "-" (total_removed) }
            }
            div class="rollup-body" {
                @for (path, a) in &by_path {
                    @let short_path = std::path::Path::new(path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    @let badge_cls = if a.is_new && a.turns == 1 { "new" } else { "edit" };
                    @let badge_label = if a.is_new && a.turns == 1 { "new" } else { "edit" };
                    div class="file-row" {
                        span class={ "badge " (badge_cls) } { (badge_label) }
                        span class="file-name" title=(path) { (short_path) }
                        span class="add" { "+" (a.added) }
                        span class="rem" { "-" (a.removed) }
                    }
                }
            }
        }
    }
    .into_string()
}

/// Compact action card combining a tool_call with its tool_result. The match
/// at the top selects per-tool rendering; the wrapping `.turn.action` is
/// uniform so CSS can style state (error / new) via attributes.
pub(super) fn render_action_card(
    call: Option<&EventRecord>,
    result: &EventRecord,
    anchor: i64,
) -> String {
    let tool = call
        .and_then(|c| c.payload.get("tool"))
        .or_else(|| result.payload.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let args = call
        .and_then(|c| c.payload.get("args"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let summary = result.payload.get("summary").and_then(|v| v.as_str()).unwrap_or("");
    let is_error = result.payload.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let data = result.payload.get("data");

    let (inner, data_tool, data_new) = match tool.as_str() {
        "shell" => render_shell_card(&args, data),
        "fs_read" => render_fs_read_card(&args, data),
        "fs_write" => render_fs_write_card(&args, data),
        "apply_patch" => render_apply_patch_card(data),
        "update_plan" => render_update_plan_card(data),
        t if t.starts_with("hook:") => render_hook_card(t, &args, data, is_error),
        _ => render_generic_card(&tool, summary),
    };

    let extra_cls = if is_error { " action-error" } else { "" };
    html! {
        div class={ "turn action" (extra_cls) } data-id=(anchor) data-tool=(data_tool) data-new=[data_new.then_some("true")] {
            (PreEscaped(inner))
        }
    }
    .into_string()
}

fn render_shell_card(
    args: &serde_json::Value,
    data: Option<&serde_json::Value>,
) -> (String, &'static str, bool) {
    let cmd = args.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    let exit = data.and_then(|d| d.get("exit_code")).map(|x| x.to_string());
    let stdout = data.and_then(|d| d.get("stdout")).and_then(|v| v.as_str()).unwrap_or("");
    let stderr = data.and_then(|d| d.get("stderr")).and_then(|v| v.as_str()).unwrap_or("");
    let backend = data.and_then(|d| d.get("backend")).and_then(|v| v.as_str()).unwrap_or("");
    let output_section = render_inline_output(stdout, stderr);
    let inner = html! {
        div class="action-head" {
            span class="action-tool" { "$" }
            " "
            code class="action-cmd" { (cmd) }
            " "
            span class="chip-spacer" {}
            @if let Some(e) = &exit { span class="action-chip" { "exit " (e) } }
            @if !backend.is_empty() { span class="action-chip muted-chip" { (backend) } }
        }
        (PreEscaped(output_section))
    }
    .into_string();
    (inner, "shell", false)
}

fn render_fs_read_card(
    args: &serde_json::Value,
    data: Option<&serde_json::Value>,
) -> (String, &'static str, bool) {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
    let bytes = data.and_then(|d| d.get("bytes")).map(|x| x.to_string());
    let preview = data
        .and_then(|d| d.get("content"))
        .and_then(|v| v.as_str())
        .map(|c| render_inline_output(c, ""))
        .unwrap_or_default();
    let inner = html! {
        div class="action-head" {
            span class="action-verb" { "Read" }
            " "
            code class="action-path" { (path) }
            " "
            span class="chip-spacer" {}
            @if let Some(b) = &bytes { span class="action-chip" { (b) " B" } }
        }
        (PreEscaped(preview))
    }
    .into_string();
    (inner, "fs_read", false)
}

fn render_fs_write_card(
    args: &serde_json::Value,
    data: Option<&serde_json::Value>,
) -> (String, &'static str, bool) {
    let path = data
        .and_then(|d| d.get("path"))
        .and_then(|v| v.as_str())
        .or_else(|| args.get("path").and_then(|v| v.as_str()))
        .unwrap_or("?");
    let added = data.and_then(|d| d.get("lines_added")).and_then(|v| v.as_u64()).unwrap_or(0);
    let removed = data.and_then(|d| d.get("lines_removed")).and_then(|v| v.as_u64()).unwrap_or(0);
    let is_new = data.and_then(|d| d.get("is_new")).and_then(|v| v.as_bool()).unwrap_or(false);
    let verb = if is_new { "Created" } else { "Edited" };
    let unified = data.and_then(|d| d.get("diff_unified")).and_then(|v| v.as_str()).unwrap_or("");
    let diff_block = render_inline_diff(unified);
    let inner = html! {
        div class="action-head" {
            span class="action-verb" { (verb) }
            " "
            code class="action-path" { (path) }
            " "
            span class="chip-spacer" {}
            span class="add" { "+" (added) }
            " "
            span class="rem" { "-" (removed) }
        }
        (PreEscaped(diff_block))
    }
    .into_string();
    (inner, "fs_write", is_new)
}

fn render_apply_patch_card(
    data: Option<&serde_json::Value>,
) -> (String, &'static str, bool) {
    let files = data.and_then(|d| d.get("files")).and_then(|v| v.as_array());
    let total_add = data.and_then(|d| d.get("lines_added")).and_then(|v| v.as_u64()).unwrap_or(0);
    let total_rem = data.and_then(|d| d.get("lines_removed")).and_then(|v| v.as_u64()).unwrap_or(0);
    let n_files = files.map(|f| f.len()).unwrap_or(0);
    let any_new = files
        .map(|arr| arr.iter().any(|f| f.get("status").and_then(|s| s.as_str()) == Some("added")))
        .unwrap_or(false);
    let inner = html! {
        div class="action-head" {
            @if n_files == 1 {
                @let f = &files.unwrap()[0];
                @let status = f.get("status").and_then(|v| v.as_str()).unwrap_or("modified");
                @let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                @let verb = match status {
                    "added" => "Created",
                    "deleted" => "Deleted",
                    "moved" => "Moved",
                    _ => "Patched",
                };
                span class="action-verb" { (verb) }
                " "
                code class="action-path" { (path) }
            } @else {
                span class="action-verb" { "Patched" }
                " "
                code class="action-path" { (n_files) " files" }
            }
            " "
            span class="chip-spacer" {}
            span class="add" { "+" (total_add) }
            " "
            span class="rem" { "-" (total_rem) }
        }
        // One diff block per file. Mirrors fs_write's renderer so the eye sees
        // the same coloring/layout.
        @if let Some(arr) = files {
            @for f in arr {
                @let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                @let status = f.get("status").and_then(|v| v.as_str()).unwrap_or("modified");
                @let added = f.get("lines_added").and_then(|v| v.as_u64()).unwrap_or(0);
                @let removed = f.get("lines_removed").and_then(|v| v.as_u64()).unwrap_or(0);
                @let diff = f.get("diff_unified").and_then(|v| v.as_str()).unwrap_or("");
                @let sclass = if status == "added" { "new" } else { "edit" };
                @if n_files > 1 {
                    div class="apply-file-head" {
                        span class={ "badge " (sclass) } { (status) }
                        " "
                        code class="action-path" { (path) }
                        " "
                        span class="chip-spacer" {}
                        span class="add" { "+" (added) }
                        " "
                        span class="rem" { "-" (removed) }
                    }
                }
                (PreEscaped(render_inline_diff(diff)))
            }
        }
    }
    .into_string();
    (inner, "apply_patch", any_new)
}

fn render_update_plan_card(
    data: Option<&serde_json::Value>,
) -> (String, &'static str, bool) {
    let plan = data.and_then(|d| d.get("plan")).and_then(|p| p.as_array());
    let note = data.and_then(|d| d.get("note")).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let total = plan.map(|p| p.len()).unwrap_or(0);
    let done = plan
        .map(|p| {
            p.iter()
                .filter(|s| s.get("status").and_then(|v| v.as_str()) == Some("completed"))
                .count()
        })
        .unwrap_or(0);
    let inner = html! {
        div class="action-head" {
            span class="action-verb" { "Plan" }
            " "
            span class="chip-spacer" {}
            span class="action-chip" { (done) "/" (total) }
        }
        @if let Some(n) = note { div class="plan-note muted small" { (n) } }
        ul class="plan-list inline-plan" {
            @if let Some(arr) = plan {
                @for s in arr {
                    @let text = s.get("step").and_then(|v| v.as_str()).unwrap_or("?");
                    @let status = s.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                    @let (cls, glyph) = match status {
                        "completed" => ("plan-step-done", "✓"),
                        "in_progress" => ("plan-step-progress", "▶"),
                        _ => ("plan-step-pending", "·"),
                    };
                    li class={ "plan-step " (cls) } {
                        span class="plan-glyph" { (glyph) }
                        span class="plan-text" { (text) }
                    }
                }
            }
        }
    }
    .into_string();
    (inner, "update_plan", false)
}

fn render_hook_card(
    tool: &str,
    args: &serde_json::Value,
    data: Option<&serde_json::Value>,
    is_error: bool,
) -> (String, &'static str, bool) {
    let label = tool.trim_start_matches("hook:");
    let exit = data.and_then(|d| d.get("exit_code")).map(|x| x.to_string());
    let stdout = data.and_then(|d| d.get("stdout")).and_then(|v| v.as_str()).unwrap_or("");
    let stderr = data.and_then(|d| d.get("stderr")).and_then(|v| v.as_str()).unwrap_or("");
    let cmd = args.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
    let exit_cls = if is_error { "action-chip err-chip" } else { "action-chip" };
    let output = render_inline_output(stdout, stderr);
    let inner = html! {
        div class="action-head" {
            span class="action-verb" { "⚙ hook · " (label) }
            " "
            code class="action-cmd" { (cmd) }
            " "
            span class="chip-spacer" {}
            @if let Some(e) = &exit { span class=(exit_cls) { "exit " (e) } }
        }
        (PreEscaped(output))
    }
    .into_string();
    (inner, "hook", false)
}

fn render_generic_card(tool: &str, summary: &str) -> (String, &'static str, bool) {
    let inner = html! {
        div class="action-head" {
            span class="action-verb" { (tool) }
            " " (summary)
        }
    }
    .into_string();
    (inner, "other", false)
}

/// Render shell stdout/stderr inline. The whole content is in one block,
/// clipped via CSS max-height when long; a "show N more" button removes the
/// clip without splitting the visual block.
pub(super) fn render_inline_output(stdout: &str, stderr: &str) -> String {
    fn block(text: &str, extra_cls: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        const VISIBLE: usize = 6;
        let total = text.lines().count();
        let extra = if extra_cls.is_empty() {
            String::new()
        } else {
            format!(" {extra_cls}")
        };
        if total <= VISIBLE {
            return format!(r#"<pre class="action-out{extra}">{}</pre>"#, html_escape(text));
        }
        let more = total - VISIBLE;
        format!(
            r#"<div class="output-wrap clipped" data-visible="{VISIBLE}"><pre class="action-out{extra}">{body}</pre><button type="button" class="show-more-btn">show {more} more line{plural}</button></div>"#,
            body = html_escape(text),
            plural = if more > 1 { "s" } else { "" },
        )
    }
    let mut s = String::new();
    s.push_str(&block(stdout, ""));
    s.push_str(&block(stderr, "action-err"));
    s
}

/// Inline diff: ≤20 lines fully visible, else clipped to ~12 lines with a
/// "show N more" button that unclips the same wrapper.
pub(super) fn render_inline_diff(unified: &str) -> String {
    if unified.is_empty() {
        return String::new();
    }
    let total = unified.lines().count();
    let body = colorize_unified(unified);
    if total <= 20 {
        return format!(r#"<div class="diff-body">{body}</div>"#);
    }
    let more = total - 12;
    format!(
        r#"<div class="output-wrap diff-wrap clipped" data-visible="12"><div class="diff-body">{body}</div><button type="button" class="show-more-btn">show {more} more line{plural} of diff</button></div>"#,
        plural = if more > 1 { "s" } else { "" },
    )
}

/// Render a unified diff as a 3-column grid: (old line, new line, text).
/// Mirrors GitHub's diff view: lines starting with `+` only get a new-side
/// number, `-` only old-side, context gets both.
pub(super) fn colorize_unified(diff: &str) -> String {
    let mut out = String::with_capacity(diff.len() + 64);
    let mut old_line: u32 = 1;
    let mut new_line: u32 = 1;
    for raw in diff.lines() {
        let (class, marker, body, old_num, new_num) = if let Some(rest) = raw.strip_prefix('+') {
            let s = ("add", "+", rest.to_string(), None, Some(new_line));
            new_line += 1;
            s
        } else if let Some(rest) = raw.strip_prefix('-') {
            let s = ("rem", "-", rest.to_string(), Some(old_line), None);
            old_line += 1;
            s
        } else {
            let rest = raw.strip_prefix(' ').unwrap_or(raw).to_string();
            let s = ("ctx", " ", rest, Some(old_line), Some(new_line));
            old_line += 1;
            new_line += 1;
            s
        };
        let old_str = old_num.map(|n| n.to_string()).unwrap_or_default();
        let new_str = new_num.map(|n| n.to_string()).unwrap_or_default();
        out.push_str(&format!(
            r#"<div class="d-line d-{class}"><span class="ln ln-old">{old_str}</span><span class="ln ln-new">{new_str}</span><span class="d-marker">{marker}</span><span class="d-text">{}</span></div>"#,
            html_escape(&body),
        ));
    }
    out
}
