//! § T2.4 — declarative YAML recipes.
//!
//! A *recipe* is a YAML file in `<data_dir>/recipes/*.yaml` that describes
//! one recurring task: cron trigger + the task spec (goal, workdir,
//! sandbox, routing, max_steps). At daemon startup every recipe file is
//! parsed and upserted into the schedule store with a deterministic id
//! (`recipe:<name>`) so re-runs are idempotent and the user can spot
//! recipe-owned schedules in the SPA.
//!
//! Inspired by Goose Recipes — the value is that recipes are
//! version-controllable (commit them to a repo), shareable (drop a file
//! in the dir), and discoverable (one file per recipe). Multi-step
//! sequencing (one task per step, in order) is deferred to v2.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Single-step recipe — one cron trigger, one task spec. v1 covers the
/// common case ("nightly audit", "every-hour deps check"); multi-step
/// recipes will reuse `name` as a prefix and emit `recipe:<name>:<step>`
/// schedule ids.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Unique kebab-case id. Used to derive the schedule id
    /// (`recipe:<name>`). Must match the filename stem (enforced at load).
    pub name: String,
    /// One-liner human-readable description. Surfaced in the UI.
    #[serde(default)]
    pub description: String,
    /// Cron expression. Both 5-field (`min hour dom mon dow`) and 6-field
    /// (with seconds) are accepted; matches the scheduler grammar.
    pub cron: String,
    /// Goal handed to the agent loop. Templating is intentionally NOT
    /// supported in v1 — keep the recipe literal until we have a clear
    /// case for variables.
    pub goal: String,
    /// Working directory the agent runs in. Required.
    pub workdir: String,
    /// Sandbox backend hint. Defaults to `native`.
    #[serde(default = "default_sandbox")]
    pub sandbox: String,
    /// Network policy (`none` / `egress_only` / `full`). Defaults to
    /// `egress_only` matching the daemon default.
    #[serde(default = "default_net_policy")]
    pub net_policy: String,
    /// Routing policy (`auto`, `local_only`, `remote_only`, `model:<name>`).
    #[serde(default = "default_routing")]
    pub routing: String,
    /// Hard cap on agent iterations. 0 means daemon default.
    #[serde(default)]
    pub max_steps: u32,
    /// Pause the schedule without removing the recipe. Defaults to false.
    #[serde(default)]
    pub paused: bool,
}

fn default_sandbox() -> String {
    "native".to_string()
}
fn default_net_policy() -> String {
    "egress_only".to_string()
}
fn default_routing() -> String {
    "auto".to_string()
}

impl Recipe {
    /// Deterministic schedule id for this recipe.
    pub fn schedule_id(&self) -> String {
        format!("recipe:{}", self.name)
    }

    /// Human-readable label for the schedule list. Falls back to the
    /// recipe name when no description is provided.
    pub fn schedule_label(&self) -> String {
        if self.description.is_empty() {
            self.name.clone()
        } else {
            self.description.clone()
        }
    }

    /// Validate the recipe content. Catches problems the serde layer
    /// can't (semantic constraints like empty fields, bad cron).
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(anyhow!("recipe.name is empty"));
        }
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(anyhow!(
                "recipe.name `{}` must be kebab/snake-case alphanumeric",
                self.name
            ));
        }
        if self.goal.trim().is_empty() {
            return Err(anyhow!("recipe.goal is empty"));
        }
        if self.workdir.trim().is_empty() {
            return Err(anyhow!("recipe.workdir is empty"));
        }
        crate::scheduler::validate_cron(&self.cron)
            .with_context(|| format!("recipe `{}`", self.name))?;
        Ok(())
    }
}

/// Parse a single recipe YAML string.
pub fn parse_recipe(src: &str) -> Result<Recipe> {
    let r: Recipe = serde_yaml::from_str(src).context("failed to parse recipe YAML")?;
    r.validate()?;
    Ok(r)
}

/// Load every `*.yaml` / `*.yml` file in `dir` as a recipe. Files whose
/// name stem doesn't match `recipe.name` are rejected (forces 1:1
/// mapping and avoids silent overwrites). Invalid recipes are logged
/// and skipped — they never block daemon startup.
pub fn load_recipes_dir(dir: &Path) -> Result<Vec<Recipe>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = fs::read_dir(dir).with_context(|| format!("read_dir({})", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        if ext.as_deref() != Some("yaml") && ext.as_deref() != Some("yml") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let src = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "recipe: read failed; skipping");
                continue;
            }
        };
        let r = match parse_recipe(&src) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "recipe: parse failed; skipping");
                continue;
            }
        };
        if r.name != stem {
            tracing::warn!(
                path = %path.display(),
                file_stem = %stem,
                recipe_name = %r.name,
                "recipe: name does not match filename; skipping"
            );
            continue;
        }
        out.push(r);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_yaml() -> &'static str {
        r#"
name: nightly-deps-audit
description: Run cargo outdated nightly and summarize critical findings
cron: "0 3 * * *"
goal: Run cargo outdated, summarize anything > 1 major version behind
workdir: /repo
sandbox: docker
net_policy: egress_only
routing: local_only
max_steps: 10
"#
    }

    #[test]
    fn parses_a_well_formed_recipe() {
        let r = parse_recipe(sample_yaml()).unwrap();
        assert_eq!(r.name, "nightly-deps-audit");
        assert_eq!(r.cron, "0 3 * * *");
        assert_eq!(r.sandbox, "docker");
        assert_eq!(r.routing, "local_only");
        assert_eq!(r.max_steps, 10);
        assert!(!r.paused);
        assert_eq!(r.schedule_id(), "recipe:nightly-deps-audit");
    }

    #[test]
    fn falls_back_to_defaults_for_optional_fields() {
        let yaml = r#"
name: simple
cron: "0 * * * *"
goal: Do a thing
workdir: /tmp
"#;
        let r = parse_recipe(yaml).unwrap();
        assert_eq!(r.sandbox, "native");
        assert_eq!(r.net_policy, "egress_only");
        assert_eq!(r.routing, "auto");
        assert_eq!(r.max_steps, 0);
        assert!(!r.paused);
        assert_eq!(r.description, "");
        assert_eq!(r.schedule_label(), "simple");
    }

    #[test]
    fn rejects_empty_goal() {
        let yaml = r#"
name: bad
cron: "0 * * * *"
goal: ""
workdir: /tmp
"#;
        assert!(parse_recipe(yaml).is_err());
    }

    #[test]
    fn rejects_invalid_cron() {
        let yaml = r#"
name: bad
cron: "not a cron"
goal: do
workdir: /tmp
"#;
        assert!(parse_recipe(yaml).is_err());
    }

    #[test]
    fn rejects_non_kebab_name() {
        let yaml = r#"
name: "has spaces"
cron: "0 * * * *"
goal: do
workdir: /tmp
"#;
        assert!(parse_recipe(yaml).is_err());
    }

    #[test]
    fn load_dir_skips_non_yaml_files() {
        let dir = tempfile::tempdir().unwrap();
        // Filename stem MUST match `recipe.name` (enforced) — the sample
        // recipe's name is `nightly-deps-audit`.
        fs::write(dir.path().join("nightly-deps-audit.yaml"), sample_yaml()).unwrap();
        fs::write(dir.path().join("README.md"), "ignored").unwrap();
        let recipes = load_recipes_dir(dir.path()).unwrap();
        assert_eq!(recipes.len(), 1);
        assert_eq!(recipes[0].name, "nightly-deps-audit");
    }

    #[test]
    fn load_dir_rejects_mismatching_filename() {
        let dir = tempfile::tempdir().unwrap();
        // sample's name is `nightly-deps-audit` but we save it under
        // `other.yaml` — must be skipped.
        fs::write(dir.path().join("other.yaml"), sample_yaml()).unwrap();
        let recipes = load_recipes_dir(dir.path()).unwrap();
        assert!(recipes.is_empty());
    }

    #[test]
    fn load_dir_returns_empty_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let recipes = load_recipes_dir(&missing).unwrap();
        assert!(recipes.is_empty());
    }
}
