use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Run a git command and return its stdout as a trimmed string.
pub fn git_output(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute: git {}", args.join(" ")))?;
    parse_git_output(&output, args)
}

/// Run a git command with a specific working directory.
pub fn git_output_in(dir: &Path, args: &[&str]) -> Result<String> {
    let stdout = git_output_raw_in(dir, args)?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

pub fn git_output_raw_in(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to execute: git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(output.stdout)
}

/// Run a git command with a specific working directory and ignore stdout.
pub fn git_in(dir: &Path, args: &[&str]) -> Result<()> {
    git_output_in(dir, args).map(|_| ())
}

fn parse_git_output(output: &std::process::Output, args: &[&str]) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Read all git config entries matching a POSIX regexp pattern in a specific directory.
pub fn config_get_regexp_in(dir: &Path, pattern: &str) -> Result<Vec<(String, String)>> {
    // -z delimits entries with NUL and key/value with \n, so values
    // containing newlines survive parsing.
    let output = Command::new("git")
        .args(["config", "-z", "--get-regexp", pattern])
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to execute: git config --get-regexp {pattern}"))?;
    if !output.status.success() {
        if output.status.code() == Some(1) {
            return Ok(vec![]);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git config --get-regexp {pattern} failed: {}",
            stderr.trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .split('\0')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('\n')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect())
}

/// Read all git config entries matching a POSIX regexp pattern in one call.
pub fn config_get_regexp(pattern: &str) -> Result<Vec<(String, String)>> {
    config_get_regexp_in(&std::env::current_dir()?, pattern)
}

/// Load the first-parent commit hashes of `ref_name` into a HashSet.
/// Called once, then shared across all branch divergence checks.
pub fn first_parent_commits(dir: &Path, ref_name: &str) -> HashSet<String> {
    git_output_in(dir, &["log", "--first-parent", "--format=%H", ref_name])
        .unwrap_or_default()
        .lines()
        .map(|s| s.to_string())
        .collect()
}

pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    git_output_in(
        dir,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok()
}

pub fn remote_branch_exists(dir: &Path, branch: &str) -> bool {
    git_output_in(
        dir,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
    .is_ok()
}

pub fn branch_refs_exist(dir: &Path, branch: &str) -> Result<(bool, bool)> {
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    let refs = git_output_in(
        dir,
        &["for-each-ref", "--format=%(refname)", "--", &local, &remote],
    )?;
    Ok((
        refs.lines().any(|name| name == local),
        refs.lines().any(|name| name == remote),
    ))
}

pub fn remote_default_branch_ref(dir: &Path) -> Result<String> {
    git_output_in(
        dir,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
}

/// Check if a branch's upstream tracking ref has been deleted (gone).
/// Returns true only when the branch has a configured remote but the
/// corresponding refs/remotes/origin/<branch> no longer exists.
pub fn has_upstream_gone(dir: &Path, branch: &str) -> bool {
    let remote = git_output_in(dir, &["config", &format!("branch.{branch}.remote")]);
    match remote {
        Ok(r) if !r.is_empty() => !remote_branch_exists(dir, branch),
        _ => false,
    }
}

/// Check if a branch has diverged from main's first-parent line.
/// `first_parents` should be pre-computed via `first_parent_commits`.
pub fn has_branch_diverged(dir: &Path, first_parents: &HashSet<String>, branch: &str) -> bool {
    let branch_tip = match git_output_in(dir, &["rev-parse", branch]) {
        Ok(tip) => tip,
        Err(_) => return false,
    };
    !first_parents.contains(&branch_tip)
}

pub fn ref_oids(dir: &Path) -> Result<HashMap<String, String>> {
    let raw = git_output_in(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            "refs/heads/",
            "refs/remotes/",
        ],
    )?;
    Ok(raw
        .lines()
        .filter_map(|line| line.split_once('\0'))
        .map(|(name, oid)| (name.to_string(), oid.to_string()))
        .collect())
}

pub struct MergeTarget {
    pub commit: String,
    pub tree: String,
}

pub fn merge_target(dir: &Path, reference: &str) -> Result<MergeTarget> {
    let raw = git_output_in(
        dir,
        &[
            "rev-parse",
            &format!("{reference}^{{commit}}"),
            &format!("{reference}^{{tree}}"),
        ],
    )?;
    let (commit, tree) = raw.split_once('\n').context("missing target tree")?;
    Ok(MergeTarget {
        commit: commit.to_string(),
        tree: tree.to_string(),
    })
}

/// Check if merging `source` into `target` would be a no-op (detects squash merges).
/// Uses `git merge-tree --write-tree` (requires git 2.38+).
pub fn is_merge_noop(dir: &Path, target: &str, source: &str) -> Result<bool> {
    is_merge_noop_with_target(dir, &merge_target(dir, target)?, source)
}

pub fn is_merge_noop_with_target(dir: &Path, target: &MergeTarget, source: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["merge-tree", "--write-tree", &target.commit, source])
        .current_dir(dir)
        .output()
        .with_context(|| {
            format!(
                "failed to execute: git merge-tree {} {source}",
                target.commit
            )
        })?;
    if !output.status.success() {
        return Ok(false);
    }
    let merge_tree = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(merge_tree == target.tree)
}

/// Return (relative_date, subject) for the most recent commit in `dir`.
pub fn last_commit_info(dir: &Path) -> Option<(String, String)> {
    let raw = git_output_in(dir, &["log", "-1", "--format=%cr%x00%s"]).ok()?;
    let (date, subject) = raw.split_once('\x00')?;
    Some((date.to_string(), subject.to_string()))
}

pub fn commit_info(dir: &Path, commits: &[&str]) -> Result<HashMap<String, (String, String)>> {
    if commits.is_empty() {
        return Ok(HashMap::new());
    }
    let mut args = vec!["log", "--no-walk=unsorted", "--format=%H%x00%cr%x00%s"];
    args.extend_from_slice(commits);
    args.push("--");
    let raw = git_output_in(dir, &args)?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\0');
            Some((
                parts.next()?.to_string(),
                (parts.next()?.to_string(), parts.next()?.to_string()),
            ))
        })
        .collect())
}

/// Return commits reachable from HEAD but not from any target ref.
pub fn unique_commit_count(dir: &Path, target_refs: &[String]) -> Option<usize> {
    let mut args = vec!["rev-list", "--count", "HEAD", "--not"];
    args.extend(target_refs.iter().map(String::as_str));
    git_output_in(dir, &args).ok()?.parse::<usize>().ok()
}

/// Parse `git worktree list --porcelain` output into (path, branch) pairs.
pub fn worktree_list(dir: &Path) -> Result<Vec<(String, Option<String>)>> {
    Ok(worktree_details(dir)?
        .into_iter()
        .map(|worktree| (worktree.path, worktree.branch))
        .collect())
}

pub struct WorktreeDetails {
    pub path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
}

pub fn worktree_details(dir: &Path) -> Result<Vec<WorktreeDetails>> {
    let raw = git_output_in(dir, &["worktree", "list", "--porcelain"])?;
    let mut result = Vec::new();
    let mut current_path = None;
    let mut current_head = None;

    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
            current_head = None;
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = Some(head.to_string());
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            if let Some(path) = current_path.take() {
                let branch = branch_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch_ref)
                    .to_string();
                result.push(WorktreeDetails {
                    path,
                    branch: Some(branch),
                    head: current_head.take(),
                });
            }
        } else if line.is_empty() {
            if let Some(path) = current_path.take() {
                result.push(WorktreeDetails {
                    path,
                    branch: None,
                    head: current_head.take(),
                });
            }
        }
    }
    if let Some(path) = current_path.take() {
        result.push(WorktreeDetails {
            path,
            branch: None,
            head: current_head,
        });
    }
    Ok(result)
}

/// Execute a command, replacing the current process (Unix exec).
pub fn exec_command(program: &str, args: &[&str], dir: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = Command::new(program).args(args).current_dir(dir).exec();
    bail!("exec {} failed: {}", program, err)
}

/// Run `git worktree <args>` as a passthrough, inheriting stdio.
pub fn git_passthrough(args: &[String]) -> Result<i32> {
    let mut child_args = vec!["worktree".to_string()];
    child_args.extend_from_slice(args);
    let status = Command::new("git")
        .args(&child_args)
        .status()
        .with_context(|| format!("failed to execute: git {}", child_args.join(" ")))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) -> String {
        git_output_in(dir, args).unwrap()
    }

    fn setup_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        git(tmp.path(), &["config", "user.email", "test@example.com"]);
        git(tmp.path(), &["config", "user.name", "Test"]);
        git(tmp.path(), &["commit", "--allow-empty", "-qm", "initial"]);
        tmp
    }

    #[test]
    fn branch_refs_exist_requires_exact_local_and_remote_names() {
        let tmp = setup_repo();
        let repo = tmp.path();
        git(repo, &["branch", "feature/nested"]);
        git(
            repo,
            &["update-ref", "refs/remotes/origin/feature/nested", "HEAD"],
        );
        assert_eq!(branch_refs_exist(repo, "feature").unwrap(), (false, false));
        assert_eq!(
            branch_refs_exist(repo, "feature/nested").unwrap(),
            (true, true)
        );
        git(repo, &["branch", "local-only"]);
        git(
            repo,
            &["update-ref", "refs/remotes/origin/remote-only", "HEAD"],
        );
        assert_eq!(
            branch_refs_exist(repo, "local-only").unwrap(),
            (true, false)
        );
        assert_eq!(
            branch_refs_exist(repo, "remote-only").unwrap(),
            (false, true)
        );
    }

    #[test]
    fn commit_and_worktree_metadata_include_detached_heads() {
        let tmp = setup_repo();
        let repo = tmp.path();
        let initial = git(repo, &["rev-parse", "HEAD"]);
        let worktree_root = TempDir::new().unwrap();
        let branch_path = worktree_root.path().join("branch");
        let detached_path = worktree_root.path().join("detached");
        git(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                branch_path.to_str().unwrap(),
            ],
        );
        git(
            &branch_path,
            &[
                "commit",
                "--allow-empty",
                "-qm",
                "feature subject\ncontinued subject\n\nbody",
            ],
        );
        let feature = git(&branch_path, &["rev-parse", "HEAD"]);
        git(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                detached_path.to_str().unwrap(),
                &initial,
            ],
        );
        git(repo, &["update-ref", "refs/remotes/origin/main", &initial]);

        let refs = ref_oids(repo).unwrap();
        assert_eq!(refs.get("refs/heads/feature"), Some(&feature));
        assert_eq!(refs.get("refs/remotes/origin/main"), Some(&initial));
        let details = worktree_details(repo).unwrap();
        let detached_path = detached_path.canonicalize().unwrap();
        let detached = details
            .iter()
            .find(|worktree| {
                worktree.path == detached_path.canonicalize().unwrap().to_string_lossy()
            })
            .unwrap();
        assert_eq!(detached.branch, None);
        assert_eq!(detached.head.as_ref(), Some(&initial));
        let branched = details
            .iter()
            .find(|worktree| worktree.branch.as_deref() == Some("feature"))
            .unwrap();
        assert_eq!(branched.head.as_ref(), Some(&feature));
        let commits = commit_info(repo, &[&initial, &feature]).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits.get(&initial), last_commit_info(repo).as_ref());
        assert_eq!(
            commits.get(&feature),
            last_commit_info(&branch_path).as_ref()
        );
    }

    #[test]
    fn cached_merge_target_preserves_squash_and_conflict_checks() {
        let tmp = setup_repo();
        let repo = tmp.path();
        git(repo, &["checkout", "-qb", "feature"]);
        std::fs::write(repo.join("feature.txt"), "feature").unwrap();
        git(repo, &["add", "feature.txt"]);
        git(repo, &["commit", "-qm", "feature"]);
        git(repo, &["checkout", "-q", "main"]);
        let before_merge = merge_target(repo, "main").unwrap();
        assert!(!is_merge_noop_with_target(repo, &before_merge, "feature").unwrap());
        std::fs::write(repo.join("feature.txt"), "feature").unwrap();
        git(repo, &["add", "feature.txt"]);
        git(repo, &["commit", "-qm", "squash feature"]);
        let after_merge = merge_target(repo, "main").unwrap();
        assert!(is_merge_noop_with_target(repo, &after_merge, "feature").unwrap());
        std::fs::write(repo.join("feature.txt"), "conflict").unwrap();
        git(repo, &["commit", "-qam", "conflicting main change"]);
        assert!(
            !is_merge_noop_with_target(repo, &merge_target(repo, "main").unwrap(), "feature")
                .unwrap()
        );
    }

    #[test]
    fn config_get_regexp_in_preserves_newlines_in_values() {
        let tmp = TempDir::new().expect("failed to create tempdir");
        for args in [
            vec!["init", "-q"],
            vec!["config", "waku.command.editor", "nvim\n--clean"],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(tmp.path())
                .status()
                .expect("failed to run git");
            assert!(status.success(), "git {args:?} should succeed");
        }

        // The ambient global config may add unrelated waku.* entries; only
        // the key set above matters here.
        let entries = config_get_regexp_in(tmp.path(), r"^waku\.").unwrap();
        let value = entries
            .iter()
            .find(|(k, _)| k == "waku.command.editor")
            .map(|(_, v)| v.as_str());
        assert_eq!(value, Some("nvim\n--clean"));
    }

    #[test]
    fn unique_commit_count_excludes_union_of_target_histories() {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let repo = tmp.path();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-q", "-m", "initial"],
            vec!["checkout", "-q", "-b", "target-a"],
            vec!["commit", "--allow-empty", "-q", "-m", "target a"],
            vec!["branch", "combined"],
            vec!["checkout", "-q", "main"],
            vec!["checkout", "-q", "-b", "target-b"],
            vec!["commit", "--allow-empty", "-q", "-m", "target b"],
            vec!["checkout", "-q", "combined"],
            vec!["merge", "-q", "--no-ff", "target-b", "-m", "merge targets"],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(repo)
                .status()
                .expect("failed to run git");
            assert!(status.success(), "git {args:?} should succeed");
        }

        assert_eq!(
            unique_commit_count(repo, &["target-a".to_string(), "target-b".to_string()]),
            Some(1)
        );
    }
}
