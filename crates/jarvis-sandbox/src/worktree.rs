//! Git worktree manager. Creates throwaway worktrees for tasks.
//!
//! Strategy: if the requested base workdir is inside a git repo, we
//! `git worktree add` a fresh path at `<data_dir>/worktrees/<task-uuid>/`,
//! branching from `base_ref` (or HEAD).
//!
//! If the path is NOT a git repo, we don't create a worktree — the caller
//! just uses the path directly. This keeps the M3 surface usable with both
//! arbitrary directories and proper git repos.

use git2::{Repository, WorktreeAddOptions, WorktreePruneOptions};
use jarvis_core::TaskId;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// A worktree owned by Jarvis. `Drop` is intentionally NOT used to clean up,
/// because Drop can't be async and the caller may want to inspect the worktree
/// after the task ends. Use `WorktreeManager::cleanup` explicitly.
#[derive(Debug, Clone)]
pub struct Worktree {
    pub task_id: TaskId,
    pub path: PathBuf,
    pub branch: String,
    /// `true` if Jarvis actually created a git worktree; `false` if the path
    /// was used as-is (no git repo at workdir).
    pub managed: bool,
}

pub struct WorktreeManager {
    root: PathBuf,
}

impl WorktreeManager {
    /// `root` is typically `<data_dir>/worktrees/`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Create a worktree for `task_id`. If `source_workdir` is inside a git repo,
    /// a true `git worktree add` is performed. Otherwise the source path is
    /// returned as the worktree path unchanged.
    pub fn create(
        &self,
        task_id: TaskId,
        source_workdir: &Path,
        base_ref: Option<&str>,
    ) -> Result<Worktree, WorktreeError> {
        // Try to discover the enclosing repo.
        let repo = match Repository::discover(source_workdir) {
            Ok(r) => r,
            Err(_) => {
                debug!(
                    workdir = %source_workdir.display(),
                    "no git repo; using workdir as-is"
                );
                return Ok(Worktree {
                    task_id,
                    path: source_workdir.to_path_buf(),
                    branch: String::new(),
                    managed: false,
                });
            }
        };

        std::fs::create_dir_all(&self.root)?;
        let path = self.root.join(task_id.to_string());
        // Avoid '/' in branch names — git uses them as subdir markers in
        // `.git/worktrees/<name>` on Windows which breaks worktree creation.
        let branch_name = format!("jarvis-{}", task_id);
        let worktree_name = branch_name.clone();

        // Resolve base ref → commit.
        let base = repo.revparse_single(base_ref.unwrap_or("HEAD"))?;
        let commit = base.peel_to_commit()?;

        // Create a branch at the base commit, then add the worktree from it.
        let branch = repo.branch(&branch_name, &commit, false)?;
        let branch_ref = branch.into_reference();

        let mut opts = WorktreeAddOptions::new();
        opts.reference(Some(&branch_ref));
        let wt = repo.worktree(&worktree_name, &path, Some(&opts))?;
        info!(
            task = %task_id,
            path = %wt.path().display(),
            branch = %branch_name,
            "worktree created"
        );

        Ok(Worktree {
            task_id,
            path: wt.path().to_path_buf(),
            branch: branch_name,
            managed: true,
        })
    }

    /// Drop a managed worktree (and its branch). No-op if `wt.managed` is false.
    pub fn cleanup(&self, wt: &Worktree, source_workdir: &Path) -> Result<(), WorktreeError> {
        if !wt.managed {
            return Ok(());
        }
        // Remove the worktree directory on disk first.
        if wt.path.exists()
            && let Err(e) = std::fs::remove_dir_all(&wt.path)
        {
            warn!(error = %e, path = %wt.path.display(), "remove_dir_all failed (proceeding)");
        }
        // Tell git about it.
        let repo = Repository::discover(source_workdir)?;
        if let Ok(handle) = repo.find_worktree(&wt.branch) {
            let mut opts = WorktreePruneOptions::new();
            opts.valid(true).locked(true).working_tree(true);
            handle.prune(Some(&mut opts))?;
        }
        // Drop the branch ref.
        if let Ok(mut b) = repo.find_branch(&wt.branch, git2::BranchType::Local) {
            let _ = b.delete();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn no_git_repo_returns_unmanaged() {
        let dir = tempdir().unwrap();
        let mgr = WorktreeManager::new(dir.path().join("worktrees"));
        let wt = mgr
            .create(TaskId::new(), dir.path(), None)
            .expect("create");
        assert!(!wt.managed);
        assert_eq!(wt.path, dir.path());
    }

    #[test]
    fn worktree_added_in_real_repo() {
        let dir = tempdir().unwrap();
        // Build a tiny git repo with one commit.
        let repo = Repository::init(dir.path()).unwrap();
        {
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            let tree_id = {
                let mut idx = repo.index().unwrap();
                idx.write_tree().unwrap()
            };
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        }
        let mgr = WorktreeManager::new(dir.path().join(".jarvis").join("worktrees"));
        let id = TaskId::new();
        let wt = mgr.create(id, dir.path(), None).expect("create");
        assert!(wt.managed);
        assert!(wt.path.exists());
        assert!(wt.path.join(".git").exists());

        // Cleanup should not error.
        mgr.cleanup(&wt, dir.path()).expect("cleanup");
        assert!(!wt.path.exists());
    }
}
