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

/// Compute the default base directory for worktrees: `{parent}/{repo-name}-worktrees/`
fn worktrees_base(root: &Path) -> Result<PathBuf> {
    let repo_name = root
        .file_name()
        .with_context(|| format!("cannot get repo name from {}", root.display()))?
        .to_string_lossy();
    let parent = root
        .parent()
        .with_context(|| format!("cannot get parent of {}", root.display()))?;
    Ok(parent.join(format!("{repo_name}-worktrees")))
}

/// Compute the base directory for worktrees using config override.
pub fn worktrees_base_with_config(root: &Path, config: &[(String, String)]) -> Result<PathBuf> {
    if let Some((_, path)) = config.iter().find(|(k, _)| k == "waku.worktrees.path") {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            return Ok(p);
        }
        return Ok(root.join(p));
    }
    worktrees_base(root)
}

/// Compute the worktree path using config override.
pub fn worktree_path_with_config(
    root: &Path,
    branch: &str,
    config: &[(String, String)],
) -> Result<PathBuf> {
    let dir_name = branch.replace('/', "-");
    Ok(worktrees_base_with_config(root, config)?.join(dir_name))
}

/// Read the branch checked out in a worktree by following its `.git` file.
/// Returns None for the main worktree (`.git` is a directory) and detached HEAD.
/// Pure file reads — no git process spawn.
fn worktree_branch(wt_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(wt_path.join(".git")).ok()?;
    let gitdir = content.strip_prefix("gitdir:")?.trim();
    let gitdir_path = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        wt_path.join(gitdir)
    };
    let head = std::fs::read_to_string(gitdir_path.join("HEAD")).ok()?;
    Some(head.trim().strip_prefix("ref: refs/heads/")?.to_string())
}

/// Resolve a query to a worktree path using pre-loaded waku config.
/// When the branch lives at its conventional path (verified against the
/// worktree's checked-out branch), this avoids spawning `git worktree list`.
pub fn resolve_worktree_with_config(
    root: &Path,
    query: &str,
    config: &[(String, String)],
) -> Result<PathBuf> {
    let query_path = PathBuf::from(query);
    if query_path.is_absolute() && query_path.is_dir() {
        return Ok(query_path);
    }

    if let Ok(candidate) = worktree_path_with_config(root, query, config) {
        if candidate.is_dir() && worktree_branch(&candidate).as_deref() == Some(query) {
            return Ok(candidate);
        }
    }

    resolve_worktree_in(root, query)
}

/// Resolve a query to a worktree path.
/// Accepts: absolute path, branch name, or worktree directory name.
pub fn resolve_worktree(query: &str) -> Result<PathBuf> {
    let query_path = PathBuf::from(query);

    // 1. Absolute path — return as-is if it exists
    if query_path.is_absolute() && query_path.is_dir() {
        return Ok(query_path);
    }

    resolve_worktree_in(&repo_root()?, query)
}

fn resolve_worktree_in(root: &Path, query: &str) -> Result<PathBuf> {
    let worktrees = git::worktree_list(root)?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo_with_worktree(branch: &str, wt_dir: &str) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let root = tmp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "-q", "--initial-branch=main"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            let status = Command::new("git")
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
                .args(&args)
                .current_dir(&root)
                .status()
                .expect("failed to run git");
            assert!(status.success(), "git {args:?} should succeed");
        }
        let wt_path = tmp.path().join(wt_dir);
        let status = Command::new("git")
            .args(["worktree", "add", "-q", "-b", branch])
            .arg(&wt_path)
            .current_dir(&root)
            .status()
            .expect("failed to run git worktree add");
        assert!(status.success(), "git worktree add should succeed");
        (tmp, root, wt_path)
    }

    fn config_for(base: &Path) -> Vec<(String, String)> {
        vec![(
            "waku.worktrees.path".to_string(),
            base.to_string_lossy().to_string(),
        )]
    }

    #[test]
    fn worktree_branch_reads_checked_out_branch() {
        let (_tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        assert_eq!(worktree_branch(&wt_path).as_deref(), Some("feature/foo"));
        // The main worktree has a .git directory, not a file — must not resolve.
        assert_eq!(worktree_branch(&root), None);
    }

    #[test]
    fn resolve_worktree_with_config_hits_conventional_path() {
        let (tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        let config = config_for(&tmp.path().join("wt"));
        let resolved = resolve_worktree_with_config(&root, "feature/foo", &config).unwrap();
        assert_eq!(resolved, wt_path);
    }

    #[test]
    fn resolve_worktree_with_config_falls_back_when_dir_has_other_branch() {
        // The worktree lives at a non-conventional path, while the conventional
        // path is occupied by a worktree with a different branch.
        let (tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "elsewhere");
        let status = Command::new("git")
            .args(["worktree", "add", "-q", "-b", "other"])
            .arg(tmp.path().join("wt/feature-foo"))
            .current_dir(&root)
            .status()
            .expect("failed to run git worktree add");
        assert!(status.success());

        let config = config_for(&tmp.path().join("wt"));
        let resolved = resolve_worktree_with_config(&root, "feature/foo", &config).unwrap();
        // git worktree list reports canonicalized paths (/var → /private/var on macOS)
        assert_eq!(resolved, wt_path.canonicalize().unwrap());
    }

    #[test]
    fn resolve_worktree_with_config_falls_back_to_dir_name_match() {
        let (tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "wt/custom-name");
        let config = config_for(&tmp.path().join("wt"));
        let resolved = resolve_worktree_with_config(&root, "custom-name", &config).unwrap();
        assert_eq!(resolved, wt_path.canonicalize().unwrap());
    }

    #[test]
    fn resolve_worktree_with_config_errors_for_unknown_query() {
        let (tmp, root, _wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        let config = config_for(&tmp.path().join("wt"));
        assert!(resolve_worktree_with_config(&root, "nope", &config).is_err());
    }

    #[test]
    fn worktrees_base_default_without_config() {
        let root = Path::new("/home/user/myrepo");
        let config: Vec<(String, String)> = vec![];
        let base = worktrees_base_with_config(root, &config).unwrap();
        assert_eq!(base, PathBuf::from("/home/user/myrepo-worktrees"));
    }

    #[test]
    fn worktrees_base_absolute_path_from_config() {
        let root = Path::new("/home/user/myrepo");
        let config = vec![
            ("waku.worktrees.path".to_string(), "/tmp/worktrees".to_string()),
        ];
        let base = worktrees_base_with_config(root, &config).unwrap();
        assert_eq!(base, PathBuf::from("/tmp/worktrees"));
    }

    #[test]
    fn worktrees_base_relative_path_from_config() {
        let root = Path::new("/home/user/myrepo");
        let config = vec![
            ("waku.worktrees.path".to_string(), "../worktrees".to_string()),
        ];
        let base = worktrees_base_with_config(root, &config).unwrap();
        assert_eq!(base, PathBuf::from("/home/user/myrepo/../worktrees"));
    }

    #[test]
    fn worktree_path_with_config_uses_custom_base() {
        let root = Path::new("/home/user/myrepo");
        let config = vec![
            ("waku.worktrees.path".to_string(), "/tmp/wt".to_string()),
        ];
        let path = worktree_path_with_config(root, "feature/foo", &config).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/wt/feature-foo"));
    }
}
