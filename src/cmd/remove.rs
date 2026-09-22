use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};

use super::{cleanup_empty_dirs, print_warning, spinner};
use crate::{git, worktree};

pub fn run(query: &str, force: bool, keep_branch: bool) -> Result<()> {
    let root = worktree::repo_root()?;
    let worktrees = git::worktree_list(&root)?;
    let path = worktree::resolve_worktree_from_list(query, &worktrees)?;

    let path_str = path.to_string_lossy().to_string();
    if path_str == root.to_string_lossy().as_ref() {
        bail!("cannot remove the main worktree");
    }

    let branch = worktrees
        .iter()
        .find(|(p, _)| p == &path_str)
        .and_then(|(_, b)| b.clone());

    let waku_config = git::config_get_regexp_in(&root, r"^waku\.")?;

    // Waku artifacts are expected to differ from the checked-out revision.
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
    eprintln!("  {} Removed worktree", console::style("✔").green(),);

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
        waku_entries.contains(line) || waku_prefixes.iter().any(|p| line.starts_with(p.as_str()))
    };

    let status = match git::git_output_raw_in(
        path,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--no-renames",
        ],
    ) {
        Ok(output) => output,
        Err(_) => return true,
    };
    status_is_dirty(&status, is_waku_artifact)
}

fn status_is_dirty(status: &[u8], is_waku_artifact: impl Fn(&str) -> bool) -> bool {
    let status = match std::str::from_utf8(status) {
        Ok(status) => status,
        Err(_) => return true,
    };
    status.split_terminator('\0').any(|entry| {
        let bytes = entry.as_bytes();
        bytes.len() < 4
            || bytes[2] != b' '
            || bytes[..2].iter().any(|s| !b" MADTU?!".contains(s))
            || !is_waku_artifact(&entry[3..])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parse_errors_are_dirty_even_when_artifacts_are_excluded() {
        for status in [b"?? \xff\0".as_slice(), b"?\0", b"??\0", b"XY file\0"] {
            assert!(status_is_dirty(status, |_| true), "{status:?}");
        }
    }
}
