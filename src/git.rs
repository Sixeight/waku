use std::collections::HashSet;
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
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to execute: git {}", args.join(" ")))?;
    parse_git_output(&output, args)
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
    let output = Command::new("git")
        .args(["config", "--get-regexp", pattern])
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to execute: git config --get-regexp {pattern}"))?;
    if !output.status.success() {
        if output.status.code() == Some(1) {
            return Ok(vec![]);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git config --get-regexp {pattern} failed: {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
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

/// Check if a branch has diverged from main's first-parent line.
/// `first_parents` should be pre-computed via `first_parent_commits`.
pub fn has_branch_diverged(
    dir: &Path,
    first_parents: &HashSet<String>,
    branch: &str,
) -> bool {
    let branch_tip = match git_output_in(dir, &["rev-parse", branch]) {
        Ok(tip) => tip,
        Err(_) => return false,
    };
    !first_parents.contains(&branch_tip)
}

/// Check if merging `source` into `target` would be a no-op (detects squash merges).
/// Uses `git merge-tree --write-tree` (requires git 2.38+).
pub fn is_merge_noop(dir: &Path, target: &str, source: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["merge-tree", "--write-tree", target, source])
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to execute: git merge-tree {target} {source}"))?;
    if !output.status.success() {
        return Ok(false);
    }
    let merge_tree = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let target_tree = git_output_in(dir, &["rev-parse", &format!("{target}^{{tree}}")])?;
    Ok(merge_tree == target_tree)
}

/// Parse `git worktree list --porcelain` output into (path, branch) pairs.
pub fn worktree_list(dir: &Path) -> Result<Vec<(String, Option<String>)>> {
    let raw = git_output_in(dir, &["worktree", "list", "--porcelain"])?;
    let mut result = Vec::new();
    let mut current_path = None;

    for line in raw.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
        } else if let Some(branch_ref) = line.strip_prefix("branch ") {
            if let Some(path) = current_path.take() {
                let branch = branch_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch_ref)
                    .to_string();
                result.push((path, Some(branch)));
            }
        } else if line.is_empty() {
            if let Some(path) = current_path.take() {
                result.push((path, None));
            }
        }
    }
    if let Some(path) = current_path.take() {
        result.push((path, None));
    }
    Ok(result)
}

/// Execute a command, replacing the current process (Unix exec).
pub fn exec_command(program: &str, args: &[&str], dir: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = Command::new(program)
        .args(args)
        .current_dir(dir)
        .exec();
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
