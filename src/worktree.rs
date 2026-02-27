use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::git;

/// Get the repository root (toplevel of the main worktree).
pub fn repo_root() -> Result<PathBuf> {
    let output = git::git_output(&["rev-parse", "--git-common-dir", "--show-toplevel"])?;
    let mut lines = output.lines();
    let common_dir = lines.next().context("missing --git-common-dir output")?;
    let toplevel = lines.next().context("missing --show-toplevel output")?;

    if common_dir == ".git" {
        return Ok(PathBuf::from(toplevel));
    }

    let common_path = PathBuf::from(common_dir);
    let git_dir = if common_path.is_absolute() {
        common_path
    } else {
        PathBuf::from(toplevel).join(&common_path)
    };
    let root = git_dir
        .parent()
        .with_context(|| format!("cannot find parent of {}", git_dir.display()))?;
    Ok(root.to_path_buf())
}

/// Compute the base directory for worktrees: `{parent}/{repo-name}-worktrees/`
pub fn worktrees_base(root: &Path) -> Result<PathBuf> {
    let repo_name = root
        .file_name()
        .with_context(|| format!("cannot get repo name from {}", root.display()))?
        .to_string_lossy();
    let parent = root
        .parent()
        .with_context(|| format!("cannot get parent of {}", root.display()))?;
    Ok(parent.join(format!("{repo_name}-worktrees")))
}

/// Compute the worktree path for a given branch name.
/// Slashes in branch names are replaced with dashes.
pub fn worktree_path(root: &Path, branch: &str) -> Result<PathBuf> {
    let dir_name = branch.replace('/', "-");
    Ok(worktrees_base(root)?.join(dir_name))
}

/// Resolve a query to a worktree path.
/// Accepts: absolute path, branch name, or worktree directory name.
pub fn resolve_worktree(query: &str) -> Result<PathBuf> {
    let query_path = PathBuf::from(query);

    // 1. Absolute path — return as-is if it exists
    if query_path.is_absolute() && query_path.is_dir() {
        return Ok(query_path);
    }

    let root = repo_root()?;
    let worktrees = git::worktree_list(&root)?;

    // 2. Branch name match
    for (path, wt_branch) in &worktrees {
        if let Some(b) = wt_branch {
            if b == query {
                return Ok(PathBuf::from(path));
            }
        }
    }

    // 3. Worktree directory name match
    for (path, _) in &worktrees {
        let p = PathBuf::from(path);
        if let Some(name) = p.file_name() {
            if name.to_string_lossy() == query {
                return Ok(p);
            }
        }
    }

    bail!("no worktree found for '{query}'")
}
