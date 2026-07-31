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

/// Read the branch checked out in a worktree by following its `.git` file,
/// verifying the worktree belongs to the repository at `root`.
/// Returns None for the main worktree (`.git` is a directory), detached HEAD,
/// and worktrees linked to another repository. Pure file reads — no git
/// process spawn.
fn worktree_branch(root: &Path, wt_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(wt_path.join(".git")).ok()?;
    let gitdir = content.strip_prefix("gitdir:")?.trim();
    let gitdir_path = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        wt_path.join(gitdir)
    };
    // A shared worktrees base can hold same-named worktrees of other repos;
    // only trust worktree metadata stored under this repo's .git/worktrees.
    let owned = root.join(".git").join("worktrees").canonicalize().ok()?;
    if !gitdir_path.canonicalize().ok()?.starts_with(owned) {
        return None;
    }
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
        if candidate.is_dir() && worktree_branch(root, &candidate).as_deref() == Some(query) {
            // Match the canonicalized paths `git worktree list` reports.
            return Ok(candidate.canonicalize().unwrap_or(candidate));
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

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args([
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .expect("failed to run git");
        assert!(status.success(), "git {args:?} should succeed");
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "--initial-branch=main"]);
        git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    }

    fn init_repo_with_worktree(branch: &str, wt_dir: &str) -> (TempDir, PathBuf, PathBuf) {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let root = tmp.path().join("repo");
        init_repo(&root);
        let wt_path = tmp.path().join(wt_dir);
        git(&root, &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            wt_path.to_str().unwrap(),
        ]);
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
        assert_eq!(worktree_branch(&root, &wt_path).as_deref(), Some("feature/foo"));
        // The main worktree has a .git directory, not a file — must not resolve.
        assert_eq!(worktree_branch(&root, &root), None);
    }

    #[test]
    fn worktree_branch_returns_none_for_detached_head() {
        let (_tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        git(&wt_path, &["checkout", "-q", "--detach"]);
        assert_eq!(worktree_branch(&root, &wt_path), None);
    }

    #[test]
    fn worktree_branch_rejects_worktree_of_another_repo() {
        let (tmp, _root, wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        let other_root = tmp.path().join("other");
        init_repo(&other_root);
        // Give the other repo its own worktree so .git/worktrees exists and
        // the ownership check exercises the path-prefix comparison.
        git(&other_root, &[
            "worktree",
            "add",
            "-q",
            "-b",
            "unrelated",
            tmp.path().join("wt/unrelated").to_str().unwrap(),
        ]);
        assert_eq!(worktree_branch(&other_root, &wt_path), None);
    }

    #[test]
    fn resolve_worktree_with_config_hits_conventional_path() {
        let (tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        let config = config_for(&tmp.path().join("wt"));
        let resolved = resolve_worktree_with_config(&root, "feature/foo", &config).unwrap();
        // The fast path canonicalizes to match `git worktree list` output.
        assert_eq!(resolved, wt_path.canonicalize().unwrap());
    }

    #[test]
    fn resolve_worktree_with_config_errors_for_foreign_repo_worktree() {
        // Two repos share the same worktrees base; the query branch only has a
        // worktree in the OTHER repo. Resolution must not cross repositories.
        let (tmp, _root, _wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        let other_root = tmp.path().join("other");
        init_repo(&other_root);
        let config = config_for(&tmp.path().join("wt"));
        assert!(resolve_worktree_with_config(&other_root, "feature/foo", &config).is_err());
    }

    #[test]
    fn resolve_worktree_with_config_accepts_absolute_path_query() {
        let (tmp, root, wt_path) = init_repo_with_worktree("feature/foo", "wt/feature-foo");
        let config = config_for(&tmp.path().join("wt"));
        let resolved =
            resolve_worktree_with_config(&root, wt_path.to_str().unwrap(), &config).unwrap();
        assert_eq!(resolved, wt_path);
    }

    #[test]
    fn resolve_worktree_with_config_fast_path_spawns_no_git() {
        // Hand-crafted worktree layout with NO functioning git repo: the
        // fallback (`git worktree list`) would fail here, so success proves
        // the fast path resolved via file reads alone.
        let tmp = TempDir::new().expect("failed to create tempdir");
        let root = tmp.path().join("repo");
        let gitdir = root.join(".git/worktrees/feature-foo");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature/foo\n").unwrap();
        let wt_path = tmp.path().join("wt/feature-foo");
        std::fs::create_dir_all(&wt_path).unwrap();
        std::fs::write(
            wt_path.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let config = config_for(&tmp.path().join("wt"));
        let resolved = resolve_worktree_with_config(&root, "feature/foo", &config).unwrap();
        assert_eq!(resolved, wt_path.canonicalize().unwrap());
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
