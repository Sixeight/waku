use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::git::{self, WorktreeDetails};

pub(super) fn remove_worktree(
    root: &Path,
    expected: &WorktreeDetails,
    config: &[(String, String)],
    allow_dirty: bool,
) -> Result<()> {
    let current = git::worktree_details(root)?
        .into_iter()
        .find(|item| item.path == expected.path)
        .context("worktree is no longer registered")?;
    if expected.head.is_none() || current.head != expected.head || current.branch != expected.branch
    {
        bail!("worktree HEAD or branch changed since listing; run clean again");
    }
    if !allow_dirty && super::super::remove::is_worktree_dirty(Path::new(&expected.path), config) {
        bail!("worktree became dirty since listing; run clean again to review the changes");
    }
    git::git_in(root, &["worktree", "remove", "--force", &expected.path])
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
        assert!(remove_worktree(&root, &expected, &[], false).is_err());
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
        assert!(remove_worktree(&root, &expected, &[], true).is_err());
        assert!(path.exists());
    }

    #[test]
    fn switching_branches_at_the_same_commit_is_protected() {
        let (_dir, root, expected) = fixture();
        let path = Path::new(&expected.path);
        git::git_in(path, &["switch", "-c", "other"]).unwrap();
        assert!(remove_worktree(&root, &expected, &[], true).is_err());
        assert!(path.exists());
    }

    #[test]
    fn configured_artifacts_do_not_prevent_removal() {
        let (_dir, root, expected) = fixture();
        fs::write(Path::new(&expected.path).join("shared.lock"), "artifact").unwrap();
        let config = vec![("waku.copy.include".into(), "shared.lock".into())];
        remove_worktree(&root, &expected, &config, false).unwrap();
        assert!(!Path::new(&expected.path).exists());
    }

    #[test]
    fn explicitly_confirmed_dirty_worktrees_can_be_removed() {
        let (_dir, root, expected) = fixture();
        fs::write(Path::new(&expected.path).join("work.txt"), "confirmed").unwrap();
        remove_worktree(&root, &expected, &[], true).unwrap();
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
            assert!(remove_worktree(&root, &expected, &[], false).is_err());
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
        assert!(remove_worktree(&root, &expected, &[], true).is_err());
        assert!(Path::new(&expected.path).join("work.txt").exists());
    }

    #[test]
    fn missing_head_snapshot_does_not_allow_forced_removal() {
        let (_dir, root, mut expected) = fixture();
        expected.head = None;
        assert!(remove_worktree(&root, &expected, &[], true).is_err());
        assert!(Path::new(&expected.path).exists());
    }
}
