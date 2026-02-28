use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};

use super::{cleanup_empty_dirs, print_warning, spinner};
use crate::{git, worktree};

pub fn run(query: &str, force: bool, keep_branch: bool) -> Result<()> {
    let root = worktree::repo_root()?;
    let path = worktree::resolve_worktree(query)?;

    let path_str = path.to_string_lossy().to_string();
    if path_str == root.to_string_lossy().as_ref() {
        bail!("cannot remove the main worktree");
    }

    // Find the branch for this worktree
    let worktrees = git::worktree_list(&root)?;
    let branch = worktrees
        .iter()
        .find(|(p, _)| p == &path_str)
        .and_then(|(_, b)| b.clone());

    let waku_config = git::config_get_regexp_in(&root, r"^waku\.")?;

    // Check for real modifications (waku artifacts like symlinks make
    // git status noisy, so use diff + ls-files and exclude waku entries)
    if !force && is_worktree_dirty(&path, &waku_config) {
        bail!(
            "'{}' contains modified or untracked files, use --force to delete",
            path_str
        );
    }

    let display = branch.as_deref().unwrap_or(query);

    // Always pass --force to git because waku artifacts (symlinks, copies)
    // make the tree appear dirty. Real dirty check is done above.
    let sp = spinner("Removing worktree".into());
    git::git_output_in(&root, &["worktree", "remove", "--force", &path_str])?;
    sp.finish_and_clear();
    eprintln!(
        "  {} Removed worktree",
        console::style("✔").green(),
    );

    // Delete the branch unless --keep-branch
    if !keep_branch {
        if let Some(ref branch) = branch {
            let delete_flag = if force { "-D" } else { "-d" };
            match git::git_output_in(&root, &["branch", delete_flag, branch]) {
                Ok(_) => {
                    eprintln!(
                        "  {} Deleted branch {}",
                        console::style("✔").green(),
                        branch,
                    );
                }
                Err(e) => {
                    print_warning(&format!("failed to delete branch '{branch}'"), &e);
                }
            }
        }
    }

    // Clean up empty directories
    let base = worktree::worktrees_base_with_config(&root, &waku_config)?;
    if base.exists() {
        cleanup_empty_dirs(&base)?;
    }

    eprintln!(
        "  {} Removed {}",
        console::style("✔").green().bold(),
        console::style(display).bold(),
    );

    Ok(())
}

/// Check if a worktree has real modifications, ignoring waku artifacts.
/// Returns `true` (dirty) when git commands fail, to avoid accidental data loss.
pub fn is_worktree_dirty(path: &Path, config: &[(String, String)]) -> bool {
    let waku_entries: HashSet<&str> = super::config_values(config, "waku.link.include")
        .into_iter()
        .chain(super::config_values(config, "waku.copy.include"))
        .collect();
    let waku_prefixes: Vec<String> = waku_entries.iter().map(|e| format!("{e}/")).collect();

    let is_waku_artifact = |line: &str| -> bool {
        waku_entries.contains(line)
            || waku_prefixes.iter().any(|p| line.starts_with(p.as_str()))
    };

    let tracked = match git::git_output_in(path, &["diff", "--name-only", "HEAD"]) {
        Ok(output) => output,
        Err(_) => return true,
    };
    if tracked.lines().any(|line| !is_waku_artifact(line)) {
        return true;
    }

    let untracked = match git::git_output_in(path, &["ls-files", "--others", "--exclude-standard"])
    {
        Ok(output) => output,
        Err(_) => return true,
    };
    untracked.lines().any(|line| !is_waku_artifact(line))
}
