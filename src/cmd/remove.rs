use anyhow::{bail, Result};

use super::{cleanup_empty_dirs, print_warning, remove_waku_copies, remove_waku_symlinks, remove_worktreeinclude_files};
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

    remove_waku_symlinks(&path)?;
    remove_waku_copies(&path)?;
    remove_worktreeinclude_files(&path, &root)?;

    // Remove the worktree
    let mut remove_args = vec!["worktree", "remove"];
    if force {
        remove_args.push("--force");
    }
    remove_args.push(&path_str);
    git::git_output_in(&root, &remove_args)?;

    // Delete the branch unless --keep-branch
    if !keep_branch {
        if let Some(ref branch) = branch {
            let delete_flag = if force { "-D" } else { "-d" };
            if let Err(e) = git::git_output_in(&root, &["branch", delete_flag, branch]) {
                print_warning(&format!("failed to delete branch '{branch}'"), &e);
            }
        }
    }

    // Clean up empty directories
    let base = worktree::worktrees_base(&root)?;
    if base.exists() {
        cleanup_empty_dirs(&base)?;
    }

    let display = branch.as_deref().unwrap_or(query);
    println!("Removed: {display}");

    Ok(())
}
