use std::fs;
use std::path::Path;
use std::process::Command;

use git_waku::cmd::remove::is_worktree_dirty;
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let tmp = TempDir::new().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    fs::write(tmp.path().join("tracked.txt"), "original\n").unwrap();
    fs::create_dir(tmp.path().join("artifacts")).unwrap();
    fs::write(tmp.path().join("artifacts/tracked.txt"), "artifact\n").unwrap();
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-q", "-m", "initial"]);
    tmp
}

fn artifact_config(path: &str) -> Vec<(String, String)> {
    vec![("waku.copy.include".into(), path.into())]
}

#[test]
fn dirty_check_detects_staged_unstaged_and_untracked_changes() {
    let tmp = repository();
    let root = tmp.path();
    assert!(!is_worktree_dirty(root, &[]));

    fs::write(root.join("tracked.txt"), "modified\n").unwrap();
    assert!(is_worktree_dirty(root, &[]));
    git(root, &["add", "tracked.txt"]);
    assert!(is_worktree_dirty(root, &[]));

    fs::write(root.join("tracked.txt"), "original\n").unwrap();
    assert!(is_worktree_dirty(root, &[]));
    git(root, &["reset", "--hard", "HEAD"]);
    fs::write(root.join("untracked\nfile.txt"), "untracked\n").unwrap();
    assert!(is_worktree_dirty(root, &[]));
}

#[test]
fn dirty_check_excludes_artifacts_with_whitespace_without_hiding_other_files() {
    let tmp = repository();
    let root = tmp.path();
    let artifact = "generated 日本語\nfile.txt";
    let config = artifact_config(artifact);
    git(root, &["config", "core.quotePath", "true"]);
    fs::write(root.join(artifact), "copy\n").unwrap();
    assert!(!is_worktree_dirty(root, &config));
    fs::write(root.join("real\tchange.txt"), "user data\n").unwrap();
    assert!(is_worktree_dirty(root, &config));
}

#[test]
fn dirty_check_checks_both_sides_of_renames_across_artifact_boundary() {
    let tmp = repository();
    let root = tmp.path();
    let config = artifact_config("artifacts");
    git(root, &["config", "status.renames", "true"]);
    git(root, &["mv", "tracked.txt", "artifacts/renamed.txt"]);
    assert!(is_worktree_dirty(root, &config));

    git(root, &["reset", "--hard", "HEAD"]);
    git(root, &["mv", "artifacts/tracked.txt", "renamed.txt"]);
    assert!(is_worktree_dirty(root, &config));

    git(root, &["reset", "--hard", "HEAD"]);
    git(
        root,
        &["mv", "artifacts/tracked.txt", "artifacts/renamed.txt"],
    );
    assert!(!is_worktree_dirty(root, &config));
}

#[test]
fn dirty_check_excludes_artifact_symlinks_and_nested_copies() {
    let tmp = repository();
    let root = tmp.path();
    let mut config = artifact_config("artifacts");
    config.push(("waku.link.include".into(), "linked".into()));
    std::os::unix::fs::symlink(root.join("artifacts"), root.join("linked")).unwrap();
    fs::create_dir(root.join("artifacts/nested")).unwrap();
    fs::write(root.join("artifacts/nested/copy.txt"), "copy\n").unwrap();
    fs::write(root.join("artifacts/tracked.txt"), "overwritten\n").unwrap();
    assert!(!is_worktree_dirty(root, &config));
    fs::write(root.join("artifacts-sibling.txt"), "user data\n").unwrap();
    assert!(is_worktree_dirty(root, &config));
}

#[test]
fn dirty_check_protects_submodule_changes_even_when_configured_to_ignore_them() {
    let source = repository();
    let tmp = repository();
    let root = tmp.path();
    git(
        root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            source.path().to_str().unwrap(),
            "vendor",
        ],
    );
    git(root, &["commit", "-q", "-m", "add submodule"]);
    assert!(!is_worktree_dirty(root, &[]));
    git(root, &["config", "submodule.vendor.ignore", "all"]);

    fs::write(root.join("vendor/tracked.txt"), "modified\n").unwrap();
    assert!(is_worktree_dirty(root, &[]));
    git(&root.join("vendor"), &["reset", "--hard", "HEAD"]);
    fs::write(root.join("vendor/untracked.txt"), "user data\n").unwrap();
    assert!(is_worktree_dirty(root, &[]));
}

#[test]
fn dirty_check_fails_closed_on_git_errors() {
    let empty = TempDir::new().unwrap();
    assert!(is_worktree_dirty(empty.path(), &[]));
}
