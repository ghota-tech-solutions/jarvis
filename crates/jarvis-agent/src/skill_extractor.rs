//! § T2.2 — self-authored skills.
//!
//! Sibling to `memory_extractor`: where memories are short imperative
//! facts ("prefer cargo check over cargo build"), skills are richer
//! markdown documents that describe a *procedure* the agent figured out
//! and could reuse on a future task ("how to add a JWT-protected route
//! in this repo: read src/auth.rs, add middleware in router.rs, …").
//!
//! Post-verdict, on a passing task, we ask the LLM to distill *at most
//! one* skill candidate per task. The result lands on disk at
//! `<workdir>/.jarvis/skills/candidates/<slug>.md`. The user reviews
//! candidates manually (or via the future `/skills` SPA route) and
//! promotes the keepers by moving the file to `…/skills/active/`.
//!
//! The loader half (`load_active_skills`) walks `…/skills/active/*.md`
//! at prompt-assembly time and returns the parsed bodies; the prompt
//! builder injects them as a system message so the agent sees them on
//! every step of every task in that workdir.
//!
//! Filesystem-based on purpose: skills are meant to live alongside
//! source code (so they can be reviewed, version-controlled, and
//! shared via the repo), not in a daemon-private database.

use jarvis_core::{ChatMessage, ChatRequest, LlmProvider, TaskId};
use jarvis_ledger::{EventKind, EventRecord, Ledger};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};

const RELEVANT_EVENT_BUDGET: u32 = 20;
/// Skip extraction unless the trace had at least this many tool calls —
/// short tasks rarely produce reusable procedural knowledge.
const MIN_TOOL_CALLS_FOR_SKILL: usize = 3;
/// Cap on the combined markdown body length we write to disk. Anything
/// longer is almost certainly noise.
const SKILL_BODY_MAX_BYTES: usize = 6 * 1024;

#[derive(Debug, Deserialize)]
struct LlmSkill {
    /// kebab-case slug. Defaults to a synth from the title if missing.
    #[serde(default)]
    name: String,
    title: String,
    /// One-liner describing *when* the skill applies. Used both as
    /// human hint and as a routing keyword bag (v1: not used yet).
    trigger: String,
    /// Bulleted steps describing the procedure, free-form markdown.
    steps: String,
    /// Optional list of tools the skill relies on.
    #[serde(default)]
    tools: Vec<String>,
}

/// Active skill as read off disk for prompt injection.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSkill {
    pub name: String,
    pub title: String,
    pub trigger: String,
    pub body: String,
    pub path: PathBuf,
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').chars().take(48).collect()
}

fn build_request(events: &[EventRecord], goal: &str) -> ChatRequest {
    let system = r#"
You are a skills extractor for an autonomous coding agent. Read the
agent's recent decisions and outcomes for ONE successful task and
distill AT MOST ONE reusable *procedure* the agent should remember for
future tasks. Output JSON with these fields:

{
  "name":    "<kebab-case-slug>",
  "title":   "<short human title, < 80 chars>",
  "trigger": "<one-liner describing WHEN this skill applies>",
  "steps":   "<markdown body — a numbered list of steps the agent took>",
  "tools":   ["fs_read", "apply_patch", "..."]
}

Rules:
- Only propose a skill when the task produced GENERALISABLE knowledge
  (a recurring procedure, not a one-off observation). When in doubt,
  output `null`.
- `steps` must be 3–10 numbered items, each one sentence. Reference
  exact tool names and file paths when relevant.
- `trigger` is one sentence in the IMPERATIVE ("When adding a new gRPC
  RPC...", "When debugging a flaky cargo test...").
- Output STRICT JSON — no markdown fences, no prose. `null` (literal)
  when no skill is worth recording.
"#;
    let mut history = String::with_capacity(2048);
    history.push_str(&format!("Goal: {goal}\n\nRecent events:\n"));
    for e in events {
        let kind = e.kind.as_str();
        let pay = serde_json::to_string(&e.payload).unwrap_or_default();
        let pay = if pay.len() > 400 {
            format!("{}…", &pay[..400])
        } else {
            pay
        };
        history.push_str(&format!("- {kind}: {pay}\n"));
    }
    ChatRequest {
        messages: vec![
            ChatMessage::system(system.trim().to_string()),
            ChatMessage::user(history),
        ],
        temperature: Some(0.2),
        top_p: None,
        max_tokens: Some(800),
        stream: false,
    }
}

fn parse_skill(raw: &str) -> Option<LlmSkill> {
    let trimmed = raw.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    if json == "null" {
        return None;
    }
    match serde_json::from_str::<LlmSkill>(json) {
        Ok(s) => Some(s),
        Err(e) => {
            debug!(error = %e, raw_len = raw.len(), "skill extractor: parse failed");
            None
        }
    }
}

fn render_skill_markdown(skill: &LlmSkill, task_id: TaskId, created_at_iso: &str) -> String {
    let name = if skill.name.is_empty() {
        slugify(&skill.title)
    } else {
        slugify(&skill.name)
    };
    let tools_line = if skill.tools.is_empty() {
        String::new()
    } else {
        format!("\n## Tools\n\n{}\n", skill.tools.join(", "))
    };
    format!(
        r#"---
name: {name}
title: {title}
trigger: {trigger}
status: candidate
created_from_task: {task_id}
created_at: {created_at_iso}
---

# {title}

## Trigger

{trigger}

## Steps

{steps}
{tools_line}"#,
        name = name,
        title = skill.title.trim().replace('\n', " "),
        trigger = skill.trigger.trim().replace('\n', " "),
        task_id = task_id,
        created_at_iso = created_at_iso,
        steps = skill.steps.trim(),
        tools_line = tools_line,
    )
}

/// Best-effort post-verdict skill extraction. Writes the candidate to
/// `<workdir>/.jarvis/skills/candidates/<slug>.md` and returns the path
/// when something was written. Failures (LLM error, no skill produced,
/// duplicate path) are logged at warn level and return `None`.
pub async fn extract_for_task(
    ledger: &Ledger,
    provider: Arc<dyn LlmProvider>,
    task_id: TaskId,
    workdir: &str,
    goal: &str,
) -> Option<PathBuf> {
    let events = match ledger
        .recent_relevant_events(task_id, RELEVANT_EVENT_BUDGET)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            warn!(error = %e, "skill extractor: cannot load events");
            return None;
        }
    };
    // Only extract when the task PASSED.
    let passed = events.iter().any(|e| {
        e.kind == EventKind::Verdict
            && e.payload
                .get("verdict")
                .and_then(|v| v.as_str())
                .map(|s| s == "pass" || s == "done")
                .unwrap_or(false)
    });
    if !passed {
        return None;
    }
    // Skip trivial tasks.
    let tool_calls = events
        .iter()
        .filter(|e| e.kind == EventKind::ToolCall)
        .count();
    if tool_calls < MIN_TOOL_CALLS_FOR_SKILL {
        debug!(tool_calls, "skill extractor: trace too short");
        return None;
    }

    let req = build_request(&events, goal);
    let resp = match provider.complete(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "skill extractor: LLM call failed");
            return None;
        }
    };
    let skill = parse_skill(&resp.content)?;
    let now = chrono::Utc::now().to_rfc3339();
    let body = render_skill_markdown(&skill, task_id, &now);
    if body.len() > SKILL_BODY_MAX_BYTES {
        warn!(
            len = body.len(),
            "skill extractor: body exceeds cap; dropping"
        );
        return None;
    }
    let dir = Path::new(workdir)
        .join(".jarvis")
        .join("skills")
        .join("candidates");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(error = %e, dir = %dir.display(), "skill extractor: mkdir failed");
        return None;
    }
    let slug = if skill.name.is_empty() {
        slugify(&skill.title)
    } else {
        slugify(&skill.name)
    };
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let path = dir.join(format!("{ts}-{slug}.md"));
    if let Err(e) = std::fs::write(&path, body) {
        warn!(error = %e, path = %path.display(), "skill extractor: write failed");
        return None;
    }
    debug!(path = %path.display(), "skill extractor: wrote candidate");
    Some(path)
}

/// Parse a single skill markdown file into the in-memory `ActiveSkill`
/// shape. Returns `None` on any parse failure — skills with malformed
/// frontmatter are silently skipped rather than blocking prompt builds.
pub fn parse_active_skill(path: &Path, src: &str) -> Option<ActiveSkill> {
    let body = src.trim();
    if !body.starts_with("---") {
        return None;
    }
    let rest = &body[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let after = &rest[end + 4..];
    // Minimal frontmatter parser: `key: value` per line, no nesting.
    let mut name = String::new();
    let mut title = String::new();
    let mut trigger = String::new();
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some((k, v)) = trimmed.split_once(':') {
            let v = v.trim().to_string();
            match k.trim() {
                "name" => name = v,
                "title" => title = v,
                "trigger" => trigger = v,
                _ => {}
            }
        }
    }
    if name.is_empty() || title.is_empty() {
        return None;
    }
    Some(ActiveSkill {
        name,
        title,
        trigger,
        body: after.trim_start_matches('\n').to_string(),
        path: path.to_path_buf(),
    })
}

/// Walk `<workdir>/.jarvis/skills/active/` and return every well-formed
/// skill. Sorted by name for stable prompt output. Missing dir returns
/// an empty Vec — never an error.
pub fn load_active_skills(workdir: &str) -> Vec<ActiveSkill> {
    let dir = Path::new(workdir)
        .join(".jarvis")
        .join("skills")
        .join("active");
    if !dir.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(s) = parse_active_skill(&path, &src) {
            out.push(s);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Foo / Bar — Baz!"), "foo-bar-baz");
        assert_eq!(slugify("__weird__"), "weird");
    }

    #[test]
    fn parse_skill_handles_null() {
        assert!(parse_skill("null").is_none());
        assert!(parse_skill(" null ").is_none());
    }

    #[test]
    fn parse_skill_handles_fenced_json() {
        let raw = r#"```json
{"name":"add-grpc-rpc","title":"Add a gRPC RPC","trigger":"When adding a new RPC","steps":"1. Edit proto\n2. Regen TS","tools":["fs_write","apply_patch"]}
```"#;
        let s = parse_skill(raw).unwrap();
        assert_eq!(s.name, "add-grpc-rpc");
        assert_eq!(s.tools.len(), 2);
    }

    #[test]
    fn render_skill_markdown_has_frontmatter_and_body() {
        let s = LlmSkill {
            name: "test-skill".to_string(),
            title: "Test skill".to_string(),
            trigger: "When testing".to_string(),
            steps: "1. First\n2. Second".to_string(),
            tools: vec!["fs_read".to_string()],
        };
        let uuid = TaskId::new();
        let md = render_skill_markdown(&s, uuid, "2026-05-25T12:00:00Z");
        assert!(md.starts_with("---\n"));
        assert!(md.contains("name: test-skill"));
        assert!(md.contains("# Test skill"));
        assert!(md.contains("## Steps"));
        assert!(md.contains("1. First"));
        assert!(md.contains("## Tools"));
        assert!(md.contains("fs_read"));
    }

    #[test]
    fn parse_active_skill_reads_frontmatter_and_body() {
        let src = r#"---
name: my-skill
title: My skill
trigger: When the moon is full
status: active
---

# My skill body

content here
"#;
        let p = std::path::PathBuf::from("/tmp/my.md");
        let s = parse_active_skill(&p, src).unwrap();
        assert_eq!(s.name, "my-skill");
        assert_eq!(s.title, "My skill");
        assert_eq!(s.trigger, "When the moon is full");
        assert!(s.body.contains("# My skill body"));
    }

    #[test]
    fn parse_active_skill_rejects_no_frontmatter() {
        let src = "# Just a markdown file without frontmatter\n";
        let p = std::path::PathBuf::from("/tmp/x.md");
        assert!(parse_active_skill(&p, src).is_none());
    }

    #[test]
    fn load_active_skills_walks_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        let active = workdir.join(".jarvis").join("skills").join("active");
        std::fs::create_dir_all(&active).unwrap();
        std::fs::write(
            active.join("alpha.md"),
            "---\nname: alpha\ntitle: Alpha\ntrigger: when X\n---\n\nbody A\n",
        )
        .unwrap();
        std::fs::write(
            active.join("beta.md"),
            "---\nname: beta\ntitle: Beta\ntrigger: when Y\n---\n\nbody B\n",
        )
        .unwrap();
        // Not a .md — should be ignored.
        std::fs::write(active.join("readme.txt"), "ignored").unwrap();
        let skills = load_active_skills(&workdir.display().to_string());
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
    }

    #[test]
    fn load_active_skills_missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let out = load_active_skills(&tmp.path().display().to_string());
        assert!(out.is_empty());
    }
}
