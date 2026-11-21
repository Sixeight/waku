use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::git::{self, WorktreeDetails};

pub(super) fn remove_worktree_staged(
    root: &Path,
    expected: &WorktreeDetails,
    config: &Config,
    allow_dirty: bool,
) -> Result<Option<PathBuf>> {
    let Some(expected_head) = expected.head.as_deref() else {
        bail!("worktree HEAD or branch changed since listing; run clean again");
    };
    let wt_path = Path::new(&expected.path);
    match inspect_registered_worktree(root, wt_path) {
        Some((current_head, current_branch)) => {
            if current_head.as_deref() != Some(expected_head) || current_branch != expected.branch {
                bail!("worktree HEAD or branch changed since listing; run clean again");
            }
        }
        None => {
            let current = git::worktree_details(root)?
                .into_iter()
                .find(|item| item.path == expected.path)
                .context("worktree is no longer registered")?;
            if current.head.as_deref() != Some(expected_head) || current.branch != expected.branch {
                bail!("worktree HEAD or branch changed since listing; run clean again");
            }
        }
    }
    if !allow_dirty && super::super::remove::is_worktree_dirty(wt_path, config) {
        bail!("worktree became dirty since listing; run clean again to review the changes");
    }
    super::super::remove::remove_worktree_staged(root, wt_path)
}

#[cfg(test)]
fn remove_worktree(
    root: &Path,
    expected: &WorktreeDetails,
    config: &Config,
    allow_dirty: bool,
) -> Result<()> {
    if let Some(staged) = remove_worktree_staged(root, expected, config, allow_dirty)? {
        super::super::remove::remove_dir_all_parallel(&staged);
    }
    Ok(())
}

/// Inspect a single linked worktree's `(head_oid, branch)` directly from its
/// `.git` linkage and admin directory under `root/.git/worktrees/`, avoiding
/// an O(N) `git worktree list` spawn on every removal. Returns `None` to fall
/// back to `git worktree list` for unexpected layouts or missing refs.
fn inspect_registered_worktree(
    root: &Path,
    wt_path: &Path,
) -> Option<(Option<String>, Option<String>)> {
    let dot_git = wt_path.join(".git");
    let content = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir_raw = content.strip_prefix("gitdir:")?.trim();
    let gitdir = if Path::new(gitdir_raw).is_absolute() {
        std::path::PathBuf::from(gitdir_raw)
    } else {
        wt_path.join(gitdir_raw)
    };
    let gitdir_canon = gitdir.canonicalize().ok()?;
    let worktrees_dir = root.join(".git").join("worktrees").canonicalize().ok()?;
    if gitdir_canon.parent() != Some(worktrees_dir.as_path()) {
        return None;
    }

    // Verify back-pointer so a recreated unregistered directory is rejected.
    let back_raw = std::fs::read_to_string(gitdir_canon.join("gitdir")).ok()?;
    let back_trimmed = back_raw.trim();
    let back_path = if Path::new(back_trimmed).is_absolute() {
        std::path::PathBuf::from(back_trimmed)
    } else {
        gitdir_canon.join(back_trimmed)
    };
    if back_path.canonicalize().ok()? != dot_git.canonicalize().ok()? {
        return None;
    }

    let head_raw = std::fs::read_to_string(gitdir_canon.join("HEAD")).ok()?;
    let head_trimmed = head_raw.trim();
    if let Some(ref_name) = head_trimmed.strip_prefix("ref: ") {
        let branch = ref_name.strip_prefix("refs/heads/")?.to_string();
        let loose_ref = root.join(".git").join(ref_name);
        let head_oid = std::fs::read_to_string(loose_ref)
            .ok()
            .map(|oid| oid.trim().to_string())
            .filter(|oid| is_hex_oid(oid))
            .or_else(|| {
                git::git_output_in(wt_path, &["rev-parse", "--verify", "HEAD^{commit}"]).ok()
            })?;
        Some((Some(head_oid), Some(branch)))
    } else if is_hex_oid(head_trimmed) {
        Some((Some(head_trimmed.to_string()), None))
    } else {
        None
    }
}

fn is_hex_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, std::path::PathBuf, WorktreeDetails) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            git::git_in(&root, &args).unwrap();
        }
        fs::write(root.join("tracked.txt"), "base\n").unwrap();
        git::git_in(&root, &["add", "tracked.txt"]).unwrap();
        git::git_in(&root, &["commit", "-m", "tracked file"]).unwrap();
        let path = dir.path().join("topic");
        git::git_in(
            &root,
            &["worktree", "add", "-b", "topic", path.to_str().unwrap()],
        )
        .unwrap();
        let expected = git::worktree_details(&root)
            .unwrap()
            .into_iter()
            .find(|item| item.branch.as_deref() == Some("topic"))
            .unwrap();
        (dir, root, expected)
    }

    #[test]
    fn edits_after_selection_preserve_the_worktree_and_branch() {
        let (_dir, root, expected) = fixture();
        let path = Path::new(&expected.path);
        fs::write(path.join("work.txt"), "in progress").unwrap();
        assert!(remove_worktree(&root, &expected, &Config::empty(), false).is_err());
        assert_eq!(
            fs::read_to_string(path.join("work.txt")).unwrap(),
            "in progress"
        );
        assert!(git::git_output_in(&root, &["rev-parse", "refs/heads/topic"]).is_ok());
    }

    #[test]
    fn a_commit_after_selection_is_protected_even_with_force() {
        let (_dir, root, expected) = fixture();
        let path = Path::new(&expected.path);
        git::git_in(path, &["commit", "--allow-empty", "-m", "unmerged work"]).unwrap();
        assert!(remove_worktree(&root, &expected, &Config::empty(), true).is_err());
        assert!(path.exists());
    }

    #[test]
    fn switching_branches_at_the_same_commit_is_protected() {
        let (_dir, root, expected) = fixture();
        let path = Path::new(&expected.path);
        git::git_in(path, &["switch", "-c", "other"]).unwrap();
        assert!(remove_worktree(&root, &expected, &Config::empty(), true).is_err());
        assert!(path.exists());
    }

    #[test]
    fn configured_artifacts_do_not_prevent_removal() {
        let (_dir, root, expected) = fixture();
        fs::write(Path::new(&expected.path).join("shared.lock"), "artifact").unwrap();
        let config = Config::from_entries(vec![("waku.copy.include".into(), "shared.lock".into())]);
        remove_worktree(&root, &expected, &config, false).unwrap();
        assert!(!Path::new(&expected.path).exists());
    }

    #[test]
    fn explicitly_confirmed_dirty_worktrees_can_be_removed() {
        let (_dir, root, expected) = fixture();
        fs::write(Path::new(&expected.path).join("work.txt"), "confirmed").unwrap();
        remove_worktree(&root, &expected, &Config::empty(), true).unwrap();
        assert!(!Path::new(&expected.path).exists());
    }

    #[test]
    fn tracked_edits_after_selection_are_kept_whether_staged_or_not() {
        for staged in [false, true] {
            let (_dir, root, expected) = fixture();
            let path = Path::new(&expected.path);
            fs::write(path.join("tracked.txt"), "in progress\n").unwrap();
            if staged {
                git::git_in(path, &["add", "tracked.txt"]).unwrap();
            }
            assert!(remove_worktree(&root, &expected, &Config::empty(), false).is_err());
            assert_eq!(
                fs::read_to_string(path.join("tracked.txt")).unwrap(),
                "in progress\n"
            );
        }
    }

    #[test]
    fn an_unregistered_directory_is_not_deleted() {
        let (_dir, root, expected) = fixture();
        git::git_in(&root, &["worktree", "remove", &expected.path]).unwrap();
        fs::create_dir(&expected.path).unwrap();
        fs::write(Path::new(&expected.path).join("work.txt"), "keep").unwrap();
        assert!(remove_worktree(&root, &expected, &Config::empty(), true).is_err());
        assert!(Path::new(&expected.path).join("work.txt").exists());
    }

    #[test]
    fn missing_head_snapshot_does_not_allow_forced_removal() {
        let (_dir, root, mut expected) = fixture();
        expected.head = None;
        assert!(remove_worktree(&root, &expected, &Config::empty(), true).is_err());
        assert!(Path::new(&expected.path).exists());
    }
}
