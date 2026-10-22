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

#[test]
fn dirty_check_excludes_literal_nested_paths_without_hiding_siblings() {
    for artifact in [
        "cache/generated.txt",
        "cache/[generated]*",
        "cache/:magic",
        "cache/日本語\nfile",
    ] {
        let tmp = repository();
        let root = tmp.path();
        let config = artifact_config(artifact);
        fs::create_dir(root.join("cache")).unwrap();
        fs::write(root.join(artifact), "generated\n").unwrap();
        assert!(!is_worktree_dirty(root, &config), "{artifact}");
        fs::write(root.join("cache/user.txt"), "user data\n").unwrap();
        assert!(is_worktree_dirty(root, &config), "{artifact}");
    }
}

#[test]
fn dirty_check_keeps_literal_behavior_for_paths_git_would_normalize() {
    let tmp = repository();
    let root = tmp.path();
    fs::write(root.join("artifacts/tracked.txt"), "modified\n").unwrap();
    for artifact in [
        "",
        ".",
        "./artifacts",
        "artifacts/",
        "artifacts//tracked.txt",
        "artifacts/../artifacts",
    ] {
        assert!(
            is_worktree_dirty(root, &artifact_config(artifact)),
            "{artifact}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn dirty_check_keeps_non_utf8_user_files() {
    use std::os::unix::ffi::OsStringExt;
    let tmp = repository();
    let root = tmp.path();
    let directory = root.join("user");
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join(std::ffi::OsString::from_vec(vec![0xff])),
        "user data\n",
    )
    .unwrap();
    assert!(is_worktree_dirty(root, &artifact_config("artifacts")));
}

#[test]
fn dirty_check_keeps_case_distinct_paths() {
    let tmp = repository();
    let root = tmp.path();
    fs::write(root.join("artifacts/tracked.txt"), "modified\n").unwrap();
    git(root, &["config", "core.ignorecase", "true"]);
    assert!(is_worktree_dirty(root, &artifact_config("ARTIFACTS")));
}

#[test]
fn remove_keeps_dirty_worktrees_when_pathspec_environment_changes_matching() {
    for variable in [
        "GIT_LITERAL_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
    ] {
        let tmp = repository();
        let root = tmp.path();
        let worktree = root.join("topic");
        git(
            root,
            &[
                "worktree",
                "add",
                "-qb",
                "topic",
                worktree.to_str().unwrap(),
            ],
        );
        git(root, &["config", "waku.copy.include", "ARTIFACTS"]);
        fs::write(worktree.join("artifacts/tracked.txt"), "user data\n").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_git-waku"))
            .args(["remove", worktree.to_str().unwrap(), "--keep-branch"])
            .current_dir(root)
            .env(variable, "1")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{variable}: dirty worktree was removed"
        );
        assert!(worktree.exists(), "{variable}");
    }
}
