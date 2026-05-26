//! § T2.6 v0 — `jarvis dist build` subcommand.
//!
//! Produces a pre-configured, ready-to-ship bundle of the Jarvis CLI +
//! daemon binaries, baked with a team-specific `jarvis.toml`, optional
//! skills/recipes directories, and a manifest describing what was
//! built. The goal is to remove every "first-run configuration" step
//! for downstream teams: they receive a directory, unpack it, run
//! `bin/jarvis-daemon` and the agent already knows their providers,
//! tool dialects, MCP servers, hooks, recipes, and skills.
//!
//! v0 scope is intentionally minimal — directory bundle only, no
//! archive (tar.gz) and no signing. v1 will add `--archive tar.gz`
//! and (when Authenticode / Apple notarization access lands)
//! optional `--sign`. The manifest already carries enough metadata
//! (git rev, timestamp, platform, included artifacts) that a CI
//! pipeline can wrap the v0 output in whatever distribution format
//! it needs.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Top-level distribution manifest. Loaded from a YAML file specified
/// via `--manifest`. Keep additions backwards-compatible: every new
/// field MUST default to a sensible no-op so older manifests still
/// build cleanly.
#[derive(Debug, Deserialize)]
pub struct DistManifest {
    /// Distribution name. Used as the output directory under
    /// `target/dist/<name>/` and recorded in MANIFEST.json.
    pub name: String,
    /// Cargo target triple. Empty / unset → builds for the host.
    #[serde(default)]
    pub target: String,
    /// "release" (default) or "debug". Anything else is rejected.
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Optional bundle contents. Each subfield independently optional.
    #[serde(default)]
    pub bundle: BundleSpec,
}

fn default_profile() -> String {
    "release".to_string()
}

#[derive(Debug, Default, Deserialize)]
pub struct BundleSpec {
    /// `jarvis.toml` to ship with the binaries. One of `inline` (literal
    /// TOML content) or `path` (relative path to copy from). When both
    /// are present, `path` wins.
    #[serde(default)]
    pub config: Option<ConfigSource>,
    /// Directories or files to copy into `bundle/skills/`. Useful for
    /// shipping a curated `.skill.md` library with the distribution.
    #[serde(default)]
    pub skills: Vec<PathBuf>,
    /// Directories or files to copy into `bundle/recipes/`. Recipe YAMLs
    /// auto-loaded by the daemon at startup.
    #[serde(default)]
    pub recipes: Vec<PathBuf>,
    /// Optional README to ship alongside the binaries.
    #[serde(default)]
    pub readme: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ConfigSource {
    Inline { inline: String },
    Path { path: PathBuf },
}

/// Build-time metadata recorded in MANIFEST.json next to the binaries.
/// Downstream tooling (signing, packaging, attestation) can rely on
/// this without re-parsing the source manifest.
#[derive(Debug, Serialize)]
struct BuildManifest {
    name: String,
    target: String,
    profile: String,
    /// RFC3339 build timestamp.
    built_at: String,
    /// `git rev-parse --short HEAD` of the workspace, or "unknown".
    git_rev: String,
    /// Files written under the bundle root, relative to it. Useful for
    /// integrity verification at install time.
    artifacts: BTreeMap<String, ArtifactEntry>,
}

#[derive(Debug, Serialize)]
struct ArtifactEntry {
    size_bytes: u64,
    /// "binary", "config", "skill", "recipe", "readme", "manifest".
    kind: String,
}

/// Entry point for the `jarvis dist build` subcommand.
pub fn run_build(manifest_path: &Path) -> Result<PathBuf> {
    let raw = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
    let manifest: DistManifest = serde_yaml::from_str(&raw)
        .with_context(|| format!("parse YAML manifest at {}", manifest_path.display()))?;

    if !matches!(manifest.profile.as_str(), "release" | "debug") {
        bail!(
            "unsupported profile '{}': use 'release' (default) or 'debug'",
            manifest.profile
        );
    }
    if manifest.name.is_empty()
        || manifest
            .name
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
    {
        bail!(
            "invalid distribution name '{}': use only [a-zA-Z0-9_-]",
            manifest.name
        );
    }

    let workspace_root = workspace_root()?;
    let manifest_dir = manifest_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    println!("→ building binaries (profile={})", manifest.profile);
    cargo_build(&workspace_root, &manifest)?;

    let out_root = workspace_root
        .join("target")
        .join("dist")
        .join(&manifest.name);
    if out_root.exists() {
        std::fs::remove_dir_all(&out_root)
            .with_context(|| format!("clear stale bundle at {}", out_root.display()))?;
    }
    std::fs::create_dir_all(&out_root)?;

    let mut artifacts = BTreeMap::new();
    copy_binaries(&workspace_root, &manifest, &out_root, &mut artifacts)?;
    write_config(&manifest_dir, &manifest, &out_root, &mut artifacts)?;
    copy_bundle_dir(
        &manifest_dir,
        &manifest.bundle.skills,
        &out_root,
        "skills",
        "skill",
        &mut artifacts,
    )?;
    copy_bundle_dir(
        &manifest_dir,
        &manifest.bundle.recipes,
        &out_root,
        "recipes",
        "recipe",
        &mut artifacts,
    )?;
    if let Some(readme) = &manifest.bundle.readme {
        let src = manifest_dir.join(readme);
        let dst = out_root.join("README.md");
        std::fs::copy(&src, &dst).with_context(|| format!("copy README from {}", src.display()))?;
        record_artifact(&dst, &out_root, "readme", &mut artifacts);
    }

    let build_manifest = BuildManifest {
        name: manifest.name.clone(),
        target: manifest.target.clone(),
        profile: manifest.profile.clone(),
        built_at: chrono::Utc::now().to_rfc3339(),
        git_rev: git_short_rev(&workspace_root).unwrap_or_else(|| "unknown".to_string()),
        artifacts,
    };
    let manifest_json = serde_json::to_string_pretty(&build_manifest)?;
    let manifest_dst = out_root.join("MANIFEST.json");
    std::fs::write(&manifest_dst, &manifest_json)?;

    println!("✓ bundle ready at {}", out_root.display());
    Ok(out_root)
}

fn workspace_root() -> Result<PathBuf> {
    // The jarvis-cli binary may be invoked from anywhere; locate the
    // workspace root by walking up from the manifest CWD until we find
    // a Cargo.toml that declares `[workspace]`. Falls back to CWD if
    // none is found (the cargo build below will surface the error).
    let mut cur = std::env::current_dir().context("current_dir")?;
    loop {
        let cargo = cur.join("Cargo.toml");
        if cargo.exists() {
            let text = std::fs::read_to_string(&cargo).unwrap_or_default();
            if text.contains("[workspace]") {
                return Ok(cur);
            }
        }
        if !cur.pop() {
            // Walked past the FS root — give up and use CWD.
            return std::env::current_dir().context("workspace_root fallback");
        }
    }
}

fn cargo_build(workspace_root: &Path, manifest: &DistManifest) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(workspace_root)
        .arg("build")
        .arg("--bin")
        .arg("jarvis")
        .arg("--bin")
        .arg("jarvis-daemon");
    if manifest.profile == "release" {
        cmd.arg("--release");
    }
    if !manifest.target.is_empty() {
        cmd.arg("--target").arg(&manifest.target);
    }
    let status = cmd.status().context("invoke cargo build")?;
    if !status.success() {
        bail!("cargo build failed with status {status}");
    }
    Ok(())
}

fn binary_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn copy_binaries(
    workspace_root: &Path,
    manifest: &DistManifest,
    out_root: &Path,
    artifacts: &mut BTreeMap<String, ArtifactEntry>,
) -> Result<()> {
    let mut target_dir = workspace_root.join("target");
    if !manifest.target.is_empty() {
        target_dir = target_dir.join(&manifest.target);
    }
    target_dir = target_dir.join(if manifest.profile == "release" {
        "release"
    } else {
        "debug"
    });

    let bin_dir = out_root.join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    for stem in ["jarvis", "jarvis-daemon"] {
        let name = binary_name(stem);
        let src = target_dir.join(&name);
        let dst = bin_dir.join(&name);
        std::fs::copy(&src, &dst)
            .with_context(|| format!("copy built binary {} → {}", src.display(), dst.display(),))?;
        record_artifact(&dst, out_root, "binary", artifacts);
    }
    Ok(())
}

fn write_config(
    manifest_dir: &Path,
    manifest: &DistManifest,
    out_root: &Path,
    artifacts: &mut BTreeMap<String, ArtifactEntry>,
) -> Result<()> {
    let Some(src) = &manifest.bundle.config else {
        return Ok(());
    };
    let content = match src {
        ConfigSource::Inline { inline } => inline.clone(),
        ConfigSource::Path { path } => std::fs::read_to_string(manifest_dir.join(path))
            .with_context(|| format!("read config from {}", path.display()))?,
    };
    let dst = out_root.join("jarvis.toml");
    std::fs::write(&dst, content)?;
    record_artifact(&dst, out_root, "config", artifacts);
    Ok(())
}

/// Copy every source under `sources` into `<out_root>/<dest_subdir>/`,
/// preserving relative paths beneath each source. Files are recorded
/// with the supplied `artifact_kind`. Missing sources fail fast — a
/// silent skip would let typos ship empty bundles.
fn copy_bundle_dir(
    manifest_dir: &Path,
    sources: &[PathBuf],
    out_root: &Path,
    dest_subdir: &str,
    artifact_kind: &str,
    artifacts: &mut BTreeMap<String, ArtifactEntry>,
) -> Result<()> {
    if sources.is_empty() {
        return Ok(());
    }
    let dest = out_root.join(dest_subdir);
    std::fs::create_dir_all(&dest)?;
    for src in sources {
        let src_abs = manifest_dir.join(src);
        let meta = std::fs::metadata(&src_abs)
            .with_context(|| format!("stat bundle source {}", src_abs.display()))?;
        if meta.is_dir() {
            copy_dir_recursive(&src_abs, &dest, out_root, artifact_kind, artifacts)?;
        } else {
            let file_name = src_abs
                .file_name()
                .map(|s| s.to_owned())
                .with_context(|| format!("no file name for {}", src_abs.display()))?;
            let dst = dest.join(file_name);
            std::fs::copy(&src_abs, &dst)?;
            record_artifact(&dst, out_root, artifact_kind, artifacts);
        }
    }
    Ok(())
}

fn copy_dir_recursive(
    src: &Path,
    dst: &Path,
    bundle_root: &Path,
    artifact_kind: &str,
    artifacts: &mut BTreeMap<String, ArtifactEntry>,
) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dst_child = dst.join(entry.file_name());
        if path.is_dir() {
            std::fs::create_dir_all(&dst_child)?;
            copy_dir_recursive(&path, &dst_child, bundle_root, artifact_kind, artifacts)?;
        } else {
            std::fs::copy(&path, &dst_child)?;
            record_artifact(&dst_child, bundle_root, artifact_kind, artifacts);
        }
    }
    Ok(())
}

fn record_artifact(
    path: &Path,
    bundle_root: &Path,
    kind: &str,
    artifacts: &mut BTreeMap<String, ArtifactEntry>,
) {
    let rel = path
        .strip_prefix(bundle_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    artifacts.insert(
        rel,
        ArtifactEntry {
            size_bytes: size,
            kind: kind.to_string(),
        },
    );
}

fn git_short_rev(workspace_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .current_dir(workspace_root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_minimal_manifest() {
        let yaml = "name: acme\n";
        let m: DistManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name, "acme");
        assert_eq!(m.profile, "release");
        assert!(m.target.is_empty());
        assert!(m.bundle.skills.is_empty());
    }

    #[test]
    fn parses_full_manifest() {
        let yaml = r#"
name: team-x
target: x86_64-unknown-linux-gnu
profile: release
bundle:
  config:
    inline: |
      [daemon]
      addr = "0.0.0.0:7777"
  skills:
    - ./skills/
  recipes:
    - ./recipes/
  readme: ./README.md
"#;
        let m: DistManifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name, "team-x");
        assert_eq!(m.target, "x86_64-unknown-linux-gnu");
        assert!(matches!(
            m.bundle.config,
            Some(ConfigSource::Inline { ref inline }) if inline.contains("[daemon]")
        ));
        assert_eq!(m.bundle.skills.len(), 1);
        assert_eq!(m.bundle.recipes.len(), 1);
        assert!(m.bundle.readme.is_some());
    }

    #[test]
    fn parses_config_path_variant() {
        let yaml = r#"
name: t
bundle:
  config:
    path: ./jarvis.toml
"#;
        let m: DistManifest = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(
            m.bundle.config,
            Some(ConfigSource::Path { ref path }) if path.ends_with("jarvis.toml")
        ));
    }

    #[test]
    fn copy_bundle_dir_records_artifacts() {
        let tmp = tempdir().unwrap();
        let src_dir = tmp.path().join("src/skills");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("a.md"), "hello").unwrap();
        std::fs::write(src_dir.join("b.md"), "world").unwrap();

        let out_root = tmp.path().join("out");
        std::fs::create_dir_all(&out_root).unwrap();
        let mut artifacts = BTreeMap::new();
        copy_bundle_dir(
            tmp.path(),
            &[PathBuf::from("src/skills")],
            &out_root,
            "skills",
            "skill",
            &mut artifacts,
        )
        .unwrap();

        assert!(out_root.join("skills/a.md").exists());
        assert!(out_root.join("skills/b.md").exists());
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.values().all(|a| a.kind == "skill"));
    }

    #[test]
    fn record_artifact_normalises_path_separators() {
        let tmp = tempdir().unwrap();
        let bundle_root = tmp.path();
        let nested = bundle_root.join("a").join("b.txt");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "hi").unwrap();
        let mut artifacts = BTreeMap::new();
        record_artifact(&nested, bundle_root, "skill", &mut artifacts);
        let key = artifacts.keys().next().unwrap();
        assert!(!key.contains('\\'), "key {key:?} must use forward slashes");
        assert_eq!(key, "a/b.txt");
    }
}
