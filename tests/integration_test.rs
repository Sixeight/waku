use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Create a temporary git repository with an initial commit.
fn setup_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("failed to create tempdir");
    let repo = tmp.path().join("myrepo");
    fs::create_dir(&repo).unwrap();

    run_git(&repo, &["init", "-b", "main"]);
    run_git(&repo, &["config", "user.email", "test@test.com"]);
    run_git(&repo, &["config", "user.name", "Test"]);
    run_git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# test\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "initial"]);

    (tmp, repo)
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        status.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&status.stderr)
    );
}

fn git_waku_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-waku"))
}

fn run_waku(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(git_waku_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git-waku")
}

// --- Tests ---

#[test]
fn create_creates_worktree() {
    let (_tmp, repo) = setup_repo();

    let output = run_waku(&repo, &["create", "feature-test"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-test");
    assert!(wt_path.exists(), "worktree directory should exist");
    assert!(wt_path.join("README.md").exists(), "README.md should exist in worktree");
}

#[test]
fn create_is_idempotent() {
    let (_tmp, repo) = setup_repo();

    let output = run_waku(&repo, &["create", "feature-idem"]);
    assert!(output.status.success());

    // Second run should also succeed
    let output = run_waku(&repo, &["create", "feature-idem"]);
    assert!(
        output.status.success(),
        "second new should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-idem");
    assert!(wt_path.exists());
}

#[test]
fn create_idempotent_still_creates_symlinks() {
    let (_tmp, repo) = setup_repo();

    let node_modules = repo.join("node_modules");
    fs::create_dir(&node_modules).unwrap();
    fs::write(node_modules.join("marker"), "exists").unwrap();
    run_git(&repo, &["config", "waku.link.include", "node_modules"]);

    // First run without symlink config, then add config and run again
    run_waku(&repo, &["create", "feature-idem-link"]);

    // Delete the symlink to simulate missing link
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-idem-link");
    let link = wt_path.join("node_modules");
    if link.is_symlink() {
        fs::remove_file(&link).unwrap();
    }

    // Second run should recreate symlinks
    let output = run_waku(&repo, &["create", "feature-idem-link"]);
    assert!(
        output.status.success(),
        "idempotent new failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(link.is_symlink(), "symlink should be recreated on second run");
}

#[test]
fn create_creates_symlinks() {
    let (_tmp, repo) = setup_repo();

    // Create a directory to symlink
    let node_modules = repo.join("node_modules");
    fs::create_dir(&node_modules).unwrap();
    fs::write(node_modules.join("marker"), "exists").unwrap();

    // Configure waku.link.include
    run_git(&repo, &["config", "waku.link.include", "node_modules"]);

    let output = run_waku(&repo, &["create", "feature-link"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-link");
    let symlink = wt_path.join("node_modules");
    assert!(symlink.is_symlink(), "node_modules should be a symlink");
    assert!(
        symlink.join("marker").exists(),
        "symlinked node_modules should contain marker"
    );
}

#[test]
fn create_runs_post_create_hook() {
    let (_tmp, repo) = setup_repo();

    // Configure a hook that creates a marker file
    run_git(
        &repo,
        &["config", "waku.hook.postCreate", "touch .hook-ran"],
    );

    let output = run_waku(&repo, &["create", "feature-hook"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-hook");
    assert!(
        wt_path.join(".hook-ran").exists(),
        "post-create hook should have created .hook-ran"
    );
}

#[test]
fn create_with_from_ref() {
    let (_tmp, repo) = setup_repo();

    // Create a second commit
    fs::write(repo.join("second.txt"), "second\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "second"]);

    // Get the first commit hash
    let first_hash = Command::new("git")
        .args(["rev-parse", "HEAD~1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let hash = String::from_utf8_lossy(&first_hash.stdout).trim().to_string();

    let output = run_waku(&repo, &["create", "feature-from", "--from", &hash]);
    assert!(
        output.status.success(),
        "git-waku create --from failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-from");
    assert!(wt_path.exists());
    // The worktree should be at the first commit, so second.txt should NOT exist
    assert!(
        !wt_path.join("second.txt").exists(),
        "worktree from first commit should not have second.txt"
    );
}

#[test]
fn create_replaces_slash_in_branch_name() {
    let (_tmp, repo) = setup_repo();

    let output = run_waku(&repo, &["create", "feature/nested/branch"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo
        .parent()
        .unwrap()
        .join("myrepo-worktrees/feature-nested-branch");
    assert!(wt_path.exists(), "worktree with slash-replaced name should exist");
}

#[test]
fn create_warns_on_missing_link_source() {
    let (_tmp, repo) = setup_repo();

    // Configure a link to a nonexistent path
    run_git(&repo, &["config", "waku.link.include", "nonexistent"]);

    let output = run_waku(&repo, &["create", "feature-missing-link"]);
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Link source not found"),
        "should warn about missing source: {stderr}"
    );
}

#[test]
fn create_overwrites_existing_link_target() {
    let (_tmp, repo) = setup_repo();

    // Track a file named "mylink" in git
    fs::write(repo.join("mylink"), "tracked").unwrap();
    run_git(&repo, &["add", "mylink"]);
    run_git(&repo, &["commit", "-m", "add mylink"]);

    // Replace mylink with a directory in working tree (the link source)
    fs::remove_file(repo.join("mylink")).unwrap();
    fs::create_dir(repo.join("mylink")).unwrap();
    fs::write(repo.join("mylink").join("marker"), "source").unwrap();

    // Configure link
    run_git(&repo, &["config", "waku.link.include", "mylink"]);

    let output = run_waku(&repo, &["create", "feature-overwrite"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-overwrite");
    let target = wt_path.join("mylink");
    assert!(target.is_symlink(), "mylink should be a symlink");
    assert!(
        target.join("marker").exists(),
        "symlinked dir should contain marker"
    );
}

#[test]
fn create_overwrites_tracked_file_with_symlink_to_file() {
    let (_tmp, repo) = setup_repo();

    // Track a file that will also be the link source (e.g. Cargo.lock pattern)
    fs::write(repo.join("shared.lock"), "lock-content").unwrap();
    run_git(&repo, &["add", "shared.lock"]);
    run_git(&repo, &["commit", "-m", "add shared.lock"]);

    // Configure link — source and target are both plain files
    run_git(&repo, &["config", "waku.link.include", "shared.lock"]);

    let output = run_waku(&repo, &["create", "feature-file-overwrite"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-file-overwrite");
    let target = wt_path.join("shared.lock");
    assert!(target.is_symlink(), "shared.lock should be a symlink");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "lock-content",
        "symlinked file should have same content"
    );
}

#[test]
fn create_overwrites_tracked_directory_with_symlink() {
    let (_tmp, repo) = setup_repo();

    // Track a directory with files
    fs::create_dir(repo.join("config")).unwrap();
    fs::write(repo.join("config/app.yml"), "app: true").unwrap();
    fs::write(repo.join("config/db.yml"), "db: true").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "add config dir"]);

    // Configure link — target is a tracked directory
    run_git(&repo, &["config", "waku.link.include", "config"]);

    let output = run_waku(&repo, &["create", "feature-dir-overwrite"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-dir-overwrite");
    let target = wt_path.join("config");
    assert!(target.is_symlink(), "config should be a symlink");
    assert!(
        target.join("app.yml").exists(),
        "symlinked config should contain app.yml"
    );
    assert!(
        target.join("db.yml").exists(),
        "symlinked config should contain db.yml"
    );
}

#[test]
fn create_handles_multiple_link_includes() {
    let (_tmp, repo) = setup_repo();

    // Create two directories to symlink
    fs::create_dir(repo.join("node_modules")).unwrap();
    fs::write(repo.join("node_modules/marker"), "nm").unwrap();
    fs::create_dir(repo.join("vendor")).unwrap();
    fs::write(repo.join("vendor/marker"), "v").unwrap();

    // Configure multiple link includes
    run_git(&repo, &["config", "--add", "waku.link.include", "node_modules"]);
    run_git(&repo, &["config", "--add", "waku.link.include", "vendor"]);

    let output = run_waku(&repo, &["create", "feature-multi-link"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-multi-link");
    assert!(wt_path.join("node_modules").is_symlink(), "node_modules should be a symlink");
    assert!(wt_path.join("vendor").is_symlink(), "vendor should be a symlink");
}



#[test]
fn clean_removes_merged_worktrees() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree, add a commit, then merge it to main with --no-ff
    run_waku(&repo, &["create", "feature-merged"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-merged");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);
    run_git(&repo, &["merge", "--no-ff", "feature-merged"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(
        output.status.success(),
        "git-waku clean failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "merged worktree should be removed");
}

#[test]
fn clean_does_not_remove_undiverged_worktrees() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree but don't make any commits
    run_waku(&repo, &["create", "feature-undiverged"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    let wt_path = repo
        .parent()
        .unwrap()
        .join("myrepo-worktrees/feature-undiverged");
    assert!(
        wt_path.exists(),
        "undiverged worktree should NOT be removed"
    );
}

#[test]
fn clean_dry_run_does_not_remove() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree with a commit, then merge it
    run_waku(&repo, &["create", "feature-dry"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-dry");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);
    run_git(&repo, &["merge", "--no-ff", "feature-dry"]);

    let output = run_waku(&repo, &["clean", "--dry-run"]);
    assert!(output.status.success());

    assert!(wt_path.exists(), "dry-run should not remove worktree");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("feature-dry"),
        "dry-run should list the worktree: {stdout}"
    );
    assert!(
        stdout.contains("add feature"),
        "dry-run should show commit subject: {stdout}"
    );
}

#[test]
fn clean_dry_run_shows_dirty_worktrees() {
    let (_tmp, repo) = setup_repo();

    // Create two worktrees, merge both
    run_waku(&repo, &["create", "feature-dry-clean"]);
    let clean_path = repo.parent().unwrap().join("myrepo-worktrees/feature-dry-clean");
    fs::write(clean_path.join("f.txt"), "f\n").unwrap();
    run_git(&clean_path, &["add", "."]);
    run_git(&clean_path, &["commit", "-m", "add f"]);
    run_git(&repo, &["merge", "--no-ff", "feature-dry-clean"]);

    run_waku(&repo, &["create", "feature-dry-dirty"]);
    let dirty_path = repo.parent().unwrap().join("myrepo-worktrees/feature-dry-dirty");
    fs::write(dirty_path.join("g.txt"), "g\n").unwrap();
    run_git(&dirty_path, &["add", "."]);
    run_git(&dirty_path, &["commit", "-m", "add g"]);
    run_git(&repo, &["merge", "--no-ff", "feature-dry-dirty"]);

    // Make one dirty
    fs::write(dirty_path.join("dirty.txt"), "dirty\n").unwrap();

    let output = run_waku(&repo, &["clean", "--dry-run"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("feature-dry-clean"),
        "should show clean worktree: {stdout}"
    );
    assert!(
        stdout.contains("feature-dry-dirty"),
        "should show dirty worktree: {stdout}"
    );
    assert!(
        stdout.contains("(dirty,"),
        "should mark dirty worktree with commit info: {stdout}"
    );
    assert!(
        stdout.contains("add g"),
        "should include commit subject for dirty worktree: {stdout}"
    );
}

#[test]
fn clean_dry_run_shows_unchanged_worktrees() {
    let (_tmp, repo) = setup_repo();

    // Create an undiverged worktree (no commits after branch creation)
    run_waku(&repo, &["create", "feature-unchanged"]);

    let output = run_waku(&repo, &["clean", "--dry-run"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("feature-unchanged"),
        "should show unchanged worktree: {stdout}"
    );
    assert!(
        stdout.contains("no changes"),
        "should mark unchanged worktree with no changes: {stdout}"
    );
    assert!(
        stdout.contains("initial"),
        "should show commit subject for unchanged worktree: {stdout}"
    );
}

#[test]
fn clean_yes_skips_unchanged_worktrees() {
    let (_tmp, repo) = setup_repo();

    // Create an undiverged worktree (no commits after branch creation)
    run_waku(&repo, &["create", "feature-unchanged-yes"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    let wt_path = repo
        .parent()
        .unwrap()
        .join("myrepo-worktrees/feature-unchanged-yes");
    assert!(
        wt_path.exists(),
        "unchanged worktree should NOT be removed with --yes"
    );
}

#[test]
fn clean_does_not_remove_unmerged() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree and add a commit that's NOT merged to main
    run_waku(&repo, &["create", "feature-unmerged"]);
    let wt_path = repo
        .parent()
        .unwrap()
        .join("myrepo-worktrees/feature-unmerged");
    fs::write(wt_path.join("unmerged.txt"), "new\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "unmerged work"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());
    assert!(wt_path.exists(), "unmerged worktree should NOT be removed");
}

fn waku_path(repo: &Path, arg: &str) -> PathBuf {
    let output = run_waku(repo, &["path", arg]);
    assert!(
        output.status.success(),
        "git-waku path '{arg}' failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

#[test]
fn path_by_branch_name() {
    let (_tmp, repo) = setup_repo();
    run_waku(&repo, &["create", "feature-cd"]);

    let result = waku_path(&repo, "feature-cd");
    let expected = repo.parent().unwrap().join("myrepo-worktrees/feature-cd");
    assert_eq!(result.canonicalize().unwrap(), expected.canonicalize().unwrap());
}

#[test]
fn path_by_branch_name_with_slash() {
    let (_tmp, repo) = setup_repo();
    run_waku(&repo, &["create", "sixeight/feature-cd"]);

    let result = waku_path(&repo, "sixeight/feature-cd");
    let expected = repo.parent().unwrap().join("myrepo-worktrees/sixeight-feature-cd");
    assert_eq!(result.canonicalize().unwrap(), expected.canonicalize().unwrap());
}

#[test]
fn path_by_worktree_dir_name() {
    let (_tmp, repo) = setup_repo();
    run_waku(&repo, &["create", "sixeight/feature-dir"]);

    let result = waku_path(&repo, "sixeight-feature-dir");
    let expected = repo.parent().unwrap().join("myrepo-worktrees/sixeight-feature-dir");
    assert_eq!(result.canonicalize().unwrap(), expected.canonicalize().unwrap());
}

#[test]
fn path_by_absolute_path() {
    let (_tmp, repo) = setup_repo();
    run_waku(&repo, &["create", "feature-abs"]);

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-abs");
    let abs = wt_path.canonicalize().unwrap();

    let result = waku_path(&repo, &abs.to_string_lossy());
    assert_eq!(result.canonicalize().unwrap(), abs);
}

#[test]
fn cd_alias_works() {
    let (_tmp, repo) = setup_repo();
    run_waku(&repo, &["create", "feature-cd-alias"]);

    let output = run_waku(&repo, &["cd", "feature-cd-alias"]);
    assert!(output.status.success(), "cd alias should work");
}

#[test]
fn passthrough_to_git_worktree() {
    let (_tmp, repo) = setup_repo();

    let output = run_waku(&repo, &["list"]);
    assert!(
        output.status.success(),
        "passthrough failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("myrepo"),
        "git worktree list should show the repo: {stdout}"
    );
}

#[test]
fn cli_help_shows_commands() {
    let output = Command::new(git_waku_bin())
        .args(["--help"])
        .output()
        .expect("failed to run git-waku --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("create"));
    assert!(stdout.contains("clean"));
    assert!(stdout.contains("open"));
}

#[test]
fn clean_removes_squash_merged_worktrees() {
    let (_tmp, repo) = setup_repo();

    // Create worktree and add a commit
    run_waku(&repo, &["create", "feature-squash"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-squash");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);

    // Simulate squash merge: create the same change on main as a single commit
    fs::write(repo.join("feature.txt"), "feature\n").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "squash merge: add feature"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(
        output.status.success(),
        "git-waku clean failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !wt_path.exists(),
        "squash-merged worktree should be removed"
    );
}

#[test]
fn clean_does_not_remove_multiple_undiverged() {
    let (_tmp, repo) = setup_repo();

    run_waku(&repo, &["create", "feature-multi-1"]);
    run_waku(&repo, &["create", "feature-multi-2"]);
    run_waku(&repo, &["create", "feature-multi-3"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    let base = repo.parent().unwrap().join("myrepo-worktrees");
    assert!(base.join("feature-multi-1").exists(), "undiverged worktree 1 should NOT be removed");
    assert!(base.join("feature-multi-2").exists(), "undiverged worktree 2 should NOT be removed");
    assert!(base.join("feature-multi-3").exists(), "undiverged worktree 3 should NOT be removed");
}

#[test]
fn clean_does_not_remove_multiple_unmerged() {
    let (_tmp, repo) = setup_repo();

    for i in 1..=5 {
        let name = format!("feature-unmerged-{i}");
        run_waku(&repo, &["create", &name]);
        let wt_path = repo
            .parent()
            .unwrap()
            .join(format!("myrepo-worktrees/{name}"));
        fs::write(wt_path.join("work.txt"), format!("work {i}\n")).unwrap();
        run_git(&wt_path, &["add", "."]);
        run_git(&wt_path, &["commit", "-m", &format!("unmerged work {i}")]);
    }

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    for i in 1..=5 {
        let wt_path = repo
            .parent()
            .unwrap()
            .join(format!("myrepo-worktrees/feature-unmerged-{i}"));
        assert!(
            wt_path.exists(),
            "unmerged worktree {i} should NOT be removed"
        );
    }
}

#[test]
fn clean_removes_empty_worktrees_dir() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree, add a commit, then merge it with --no-ff
    run_waku(&repo, &["create", "feature-empty-cleanup"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-empty-cleanup");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);
    run_git(&repo, &["merge", "--no-ff", "feature-empty-cleanup"]);

    // Clean should remove the merged worktree and the base dir if it becomes empty
    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    let base = repo.parent().unwrap().join("myrepo-worktrees");
    assert!(
        !base.exists(),
        "empty worktrees base dir should be removed"
    );
}

#[test]
fn clean_warns_without_force_on_dirty_worktree() {
    let (_tmp, repo) = setup_repo();

    // Create worktree, add a commit, merge it, then make it dirty
    run_waku(&repo, &["create", "feature-dirty"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-dirty");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);
    run_git(&repo, &["merge", "--no-ff", "feature-dirty"]);

    // Create untracked file to make the worktree dirty
    fs::write(wt_path.join("untracked.txt"), "dirty\n").unwrap();

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    // Worktree should still exist (removal failed)
    assert!(wt_path.exists(), "dirty worktree should NOT be removed without --force");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--force"),
        "should suggest using --force: {stderr}"
    );
}

#[test]
fn clean_force_removes_dirty_worktree() {
    let (_tmp, repo) = setup_repo();

    // Create worktree, add a commit, merge it, then make it dirty
    run_waku(&repo, &["create", "feature-dirty-force"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-dirty-force");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);
    run_git(&repo, &["merge", "--no-ff", "feature-dirty-force"]);

    // Create untracked file to make the worktree dirty
    fs::write(wt_path.join("untracked.txt"), "dirty\n").unwrap();

    let output = run_waku(&repo, &["clean", "--yes", "--force"]);
    assert!(
        output.status.success(),
        "git-waku clean --force failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "dirty worktree should be removed with --force");
}

#[test]
fn clean_removes_detached_worktree() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree, add a commit, then detach it and delete the branch
    run_waku(&repo, &["create", "feature-detached"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-detached");
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);
    // Detach HEAD in the worktree, then delete the branch from main
    run_git(&wt_path, &["checkout", "--detach"]);
    run_git(&repo, &["branch", "-D", "feature-detached"]);

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(
        output.status.success(),
        "git-waku clean failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "detached worktree should be removed");
}

#[test]
fn clean_does_not_remove_dirty_detached_worktree() {
    let (_tmp, repo) = setup_repo();

    // Create a worktree, detach it, delete the branch
    run_waku(&repo, &["create", "feature-detached-dirty"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-detached-dirty");
    run_git(&wt_path, &["checkout", "--detach"]);
    run_git(&repo, &["branch", "-D", "feature-detached-dirty"]);

    // Make it dirty with an untracked file
    fs::write(wt_path.join("dirty.txt"), "unsaved work\n").unwrap();

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(output.status.success());

    assert!(
        wt_path.exists(),
        "dirty detached worktree should NOT be removed without --force"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning"),
        "should warn about dirty detached worktree: {stderr}"
    );
}

// --- rm tests ---

#[test]
fn remove_removes_worktree() {
    let (_tmp, repo) = setup_repo();

    run_waku(&repo, &["create", "feature-rm"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-rm");
    assert!(wt_path.exists());

    let output = run_waku(&repo, &["remove", "feature-rm"]);
    assert!(
        output.status.success(),
        "git-waku remove failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "worktree should be removed");

    // Branch should also be deleted
    let branches = Command::new("git")
        .args(["branch", "--list", "feature-rm"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let branch_list = String::from_utf8_lossy(&branches.stdout);
    assert!(
        branch_list.trim().is_empty(),
        "branch should be deleted: {branch_list}"
    );
}

#[test]
fn remove_keep_branch() {
    let (_tmp, repo) = setup_repo();

    run_waku(&repo, &["create", "feature-rm-keep"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-rm-keep");
    assert!(wt_path.exists());

    let output = run_waku(&repo, &["remove", "feature-rm-keep", "--keep-branch"]);
    assert!(
        output.status.success(),
        "git-waku remove --keep-branch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "worktree should be removed");

    // Branch should still exist
    let branches = Command::new("git")
        .args(["branch", "--list", "feature-rm-keep"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let branch_list = String::from_utf8_lossy(&branches.stdout);
    assert!(
        !branch_list.trim().is_empty(),
        "branch should still exist with --keep-branch"
    );
}

#[test]
fn remove_by_dir_name() {
    let (_tmp, repo) = setup_repo();

    run_waku(&repo, &["create", "sixeight/feature-rm-dir"]);
    let wt_path = repo
        .parent()
        .unwrap()
        .join("myrepo-worktrees/sixeight-feature-rm-dir");
    assert!(wt_path.exists());

    // Remove by directory name (slash replaced with dash)
    let output = run_waku(&repo, &["remove", "sixeight-feature-rm-dir"]);
    assert!(
        output.status.success(),
        "git-waku remove by dir name failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "worktree should be removed by dir name");
}

#[test]
fn remove_force_dirty() {
    let (_tmp, repo) = setup_repo();

    run_waku(&repo, &["create", "feature-rm-dirty"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-rm-dirty");

    // Make it dirty
    fs::write(wt_path.join("untracked.txt"), "dirty\n").unwrap();

    // Without --force should fail
    let output = run_waku(&repo, &["remove", "feature-rm-dirty"]);
    assert!(
        !output.status.success(),
        "rm should fail on dirty worktree without --force"
    );

    // With --force should succeed
    let output = run_waku(&repo, &["remove", "feature-rm-dirty", "--force"]);
    assert!(
        output.status.success(),
        "git-waku remove --force failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!wt_path.exists(), "dirty worktree should be removed with --force");
}

#[test]
fn remove_cleans_up_symlinks_before_remove() {
    let (_tmp, repo) = setup_repo();

    let node_modules = repo.join("node_modules");
    fs::create_dir(&node_modules).unwrap();
    fs::write(node_modules.join("marker"), "exists").unwrap();
    run_git(&repo, &["config", "waku.link.include", "node_modules"]);

    run_waku(&repo, &["create", "feature-rm-symlink"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-rm-symlink");
    assert!(wt_path.join("node_modules").is_symlink());

    // rm without --force should succeed because symlinks are cleaned up first
    let output = run_waku(&repo, &["remove", "feature-rm-symlink"]);
    assert!(
        output.status.success(),
        "rm should succeed with symlinks: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!wt_path.exists(), "worktree should be removed");
}

#[test]
fn clean_cleans_up_symlinks_before_remove() {
    let (_tmp, repo) = setup_repo();

    let node_modules = repo.join("node_modules");
    fs::create_dir(&node_modules).unwrap();
    fs::write(node_modules.join("marker"), "exists").unwrap();
    run_git(&repo, &["config", "waku.link.include", "node_modules"]);

    run_waku(&repo, &["create", "feature-clean-symlink"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-clean-symlink");
    assert!(wt_path.join("node_modules").is_symlink());

    // Add a commit (not touching node_modules) and merge the branch
    fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
    run_git(&wt_path, &["add", "feature.txt"]);
    run_git(&wt_path, &["commit", "-m", "add feature"]);

    // Temporarily move node_modules so merge doesn't conflict with untracked dir
    let nm_backup = repo.parent().unwrap().join("node_modules_backup");
    fs::rename(&node_modules, &nm_backup).unwrap();
    run_git(&repo, &["merge", "--no-ff", "feature-clean-symlink"]);
    fs::rename(&nm_backup, &node_modules).unwrap();

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert!(
        output.status.success(),
        "clean should succeed with symlinks: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!wt_path.exists(), "worktree should be removed");
}

// --- copy tests ---

#[test]
fn create_copies_file() {
    let (_tmp, repo) = setup_repo();

    // Create a file to copy
    fs::write(repo.join("Cargo.lock"), "lock-content").unwrap();

    // Configure waku.copy.include
    run_git(&repo, &["config", "waku.copy.include", "Cargo.lock"]);

    let output = run_waku(&repo, &["create", "feature-copy-file"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-copy-file");
    let copied = wt_path.join("Cargo.lock");
    assert!(copied.exists(), "Cargo.lock should be copied");
    assert!(!copied.is_symlink(), "Cargo.lock should NOT be a symlink");
    assert_eq!(
        fs::read_to_string(&copied).unwrap(),
        "lock-content",
        "copied file should have same content"
    );
}

#[test]
fn create_copies_directory() {
    let (_tmp, repo) = setup_repo();

    // Create a directory to copy
    fs::create_dir(repo.join(".direnv")).unwrap();
    fs::write(repo.join(".direnv/env"), "export FOO=bar").unwrap();
    fs::create_dir(repo.join(".direnv/sub")).unwrap();
    fs::write(repo.join(".direnv/sub/nested"), "nested").unwrap();

    run_git(&repo, &["config", "waku.copy.include", ".direnv"]);

    let output = run_waku(&repo, &["create", "feature-copy-dir"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-copy-dir");
    let copied = wt_path.join(".direnv");
    assert!(copied.is_dir(), ".direnv should be a directory");
    assert!(!copied.is_symlink(), ".direnv should NOT be a symlink");
    assert_eq!(
        fs::read_to_string(copied.join("env")).unwrap(),
        "export FOO=bar"
    );
    assert_eq!(
        fs::read_to_string(copied.join("sub/nested")).unwrap(),
        "nested"
    );
}

#[test]
fn create_copies_in_parallel_multiple_entries() {
    let (_tmp, repo) = setup_repo();

    // Create multiple entries
    fs::write(repo.join("file1.txt"), "content1").unwrap();
    fs::create_dir(repo.join("dir1")).unwrap();
    fs::write(repo.join("dir1/a.txt"), "a").unwrap();
    fs::write(repo.join("file2.txt"), "content2").unwrap();

    run_git(&repo, &["config", "--add", "waku.copy.include", "file1.txt"]);
    run_git(&repo, &["config", "--add", "waku.copy.include", "dir1"]);
    run_git(&repo, &["config", "--add", "waku.copy.include", "file2.txt"]);

    let output = run_waku(&repo, &["create", "feature-copy-multi"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-copy-multi");
    assert_eq!(fs::read_to_string(wt_path.join("file1.txt")).unwrap(), "content1");
    assert_eq!(fs::read_to_string(wt_path.join("dir1/a.txt")).unwrap(), "a");
    assert_eq!(fs::read_to_string(wt_path.join("file2.txt")).unwrap(), "content2");
}

#[test]
fn create_copy_warns_on_missing_source() {
    let (_tmp, repo) = setup_repo();

    run_git(&repo, &["config", "waku.copy.include", "nonexistent"]);

    let output = run_waku(&repo, &["create", "feature-copy-missing"]);
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Copy source not found"),
        "should warn about missing source: {stderr}"
    );
}

#[test]
fn remove_cleans_up_copies_before_remove() {
    let (_tmp, repo) = setup_repo();

    fs::create_dir(repo.join(".direnv")).unwrap();
    fs::write(repo.join(".direnv/env"), "export FOO=bar").unwrap();
    run_git(&repo, &["config", "waku.copy.include", ".direnv"]);

    run_waku(&repo, &["create", "feature-rm-copy"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-rm-copy");
    assert!(wt_path.join(".direnv").is_dir());

    let output = run_waku(&repo, &["remove", "feature-rm-copy"]);
    assert!(
        output.status.success(),
        "rm should succeed with copies: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!wt_path.exists(), "worktree should be removed");
}

// --- .worktreeinclude tests ---

#[test]
fn create_copies_worktreeinclude_files_by_default() {
    let (_tmp, repo) = setup_repo();

    // Create .gitignore and .worktreeinclude
    fs::write(repo.join(".gitignore"), ".env\nnode_modules/\n").unwrap();
    run_git(&repo, &["add", ".gitignore"]);
    run_git(&repo, &["commit", "-m", "add gitignore"]);

    fs::write(repo.join(".worktreeinclude"), ".env\nnode_modules/\n").unwrap();

    // Create the ignored files
    fs::write(repo.join(".env"), "SECRET=123").unwrap();
    fs::create_dir(repo.join("node_modules")).unwrap();
    fs::write(repo.join("node_modules/pkg.json"), "{}").unwrap();

    let output = run_waku(&repo, &["create", "feature-wti"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-wti");
    // .env should be copied (not symlinked)
    assert!(wt_path.join(".env").exists(), ".env should be copied");
    assert!(!wt_path.join(".env").is_symlink(), ".env should NOT be a symlink");
    assert_eq!(fs::read_to_string(wt_path.join(".env")).unwrap(), "SECRET=123");
    // node_modules should be copied
    assert!(wt_path.join("node_modules").is_dir(), "node_modules should be copied");
    assert!(!wt_path.join("node_modules").is_symlink(), "node_modules should NOT be a symlink");
    assert_eq!(
        fs::read_to_string(wt_path.join("node_modules/pkg.json")).unwrap(),
        "{}"
    );
}

#[test]
fn create_links_worktreeinclude_files_when_configured() {
    let (_tmp, repo) = setup_repo();

    fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    run_git(&repo, &["add", ".gitignore"]);
    run_git(&repo, &["commit", "-m", "add gitignore"]);

    fs::write(repo.join(".worktreeinclude"), ".env\n").unwrap();
    fs::write(repo.join(".env"), "SECRET=123").unwrap();

    // Set mode to link
    run_git(&repo, &["config", "waku.worktreeinclude", "link"]);

    let output = run_waku(&repo, &["create", "feature-wti-link"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-wti-link");
    assert!(wt_path.join(".env").is_symlink(), ".env should be a symlink");
    assert_eq!(fs::read_to_string(wt_path.join(".env")).unwrap(), "SECRET=123");
}

#[test]
fn create_ignores_worktreeinclude_when_configured() {
    let (_tmp, repo) = setup_repo();

    fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    run_git(&repo, &["add", ".gitignore"]);
    run_git(&repo, &["commit", "-m", "add gitignore"]);

    fs::write(repo.join(".worktreeinclude"), ".env\n").unwrap();
    fs::write(repo.join(".env"), "SECRET=123").unwrap();

    // Set mode to ignore
    run_git(&repo, &["config", "waku.worktreeinclude", "ignore"]);

    let output = run_waku(&repo, &["create", "feature-wti-ignore"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-wti-ignore");
    assert!(
        !wt_path.join(".env").exists(),
        ".env should NOT be present when mode is ignore"
    );
}

#[test]
fn create_worktreeinclude_only_copies_gitignored_files() {
    let (_tmp, repo) = setup_repo();

    // .worktreeinclude lists a file, but it's NOT in .gitignore
    fs::write(repo.join(".gitignore"), "").unwrap();
    run_git(&repo, &["add", ".gitignore"]);
    run_git(&repo, &["commit", "-m", "add gitignore"]);

    fs::write(repo.join(".worktreeinclude"), "tracked.txt\n").unwrap();
    fs::write(repo.join("tracked.txt"), "tracked").unwrap();
    run_git(&repo, &["add", "tracked.txt"]);
    run_git(&repo, &["commit", "-m", "add tracked file"]);

    let output = run_waku(&repo, &["create", "feature-wti-tracked"]);
    assert!(output.status.success());

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-wti-tracked");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // tracked.txt should exist from git checkout, not from waku copy
    // The key point: it should NOT be reported as "Copied" in stderr
    assert!(
        !stderr.contains("Copied tracked.txt"),
        "should not copy tracked files: {stderr}"
    );
}

#[test]
fn remove_cleans_up_worktreeinclude_copies() {
    let (_tmp, repo) = setup_repo();

    fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    run_git(&repo, &["add", ".gitignore"]);
    run_git(&repo, &["commit", "-m", "add gitignore"]);

    fs::write(repo.join(".worktreeinclude"), ".env\n").unwrap();
    fs::write(repo.join(".env"), "SECRET=123").unwrap();

    run_waku(&repo, &["create", "feature-wti-rm"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-wti-rm");
    assert!(wt_path.join(".env").exists());

    let output = run_waku(&repo, &["remove", "feature-wti-rm"]);
    assert!(
        output.status.success(),
        "rm should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!wt_path.exists());
}

// --- completions tests ---

#[test]
fn create_run_returns_worktree_path() {
    let (_tmp, repo) = setup_repo();

    let result = git_waku::cmd::create::run(
        "feature-api-path",
        git_waku::cmd::create::CreateOptions {
            quiet: true,
            root: Some(repo.clone()),
            ..Default::default()
        },
    );

    let wt_path = result.expect("run() should succeed");
    let expected = repo.parent().unwrap().join("myrepo-worktrees/feature-api-path");
    assert_eq!(wt_path, expected);
    assert!(wt_path.exists(), "returned path should exist");
    assert!(
        wt_path.join("README.md").exists(),
        "worktree should have README.md"
    );
}

#[test]
fn create_quiet_suppresses_stderr() {
    let (_tmp, repo) = setup_repo();

    let output = run_waku(&repo, &["create", "feature-quiet"]);
    assert!(output.status.success());
    let normal_stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        normal_stderr.contains("Created worktree"),
        "normal mode should have stderr output: {normal_stderr}"
    );

    // quiet mode is only available via library API, but we can verify
    // the CLI still works as expected and returns correct path
    let path_output = run_waku(&repo, &["path", "feature-quiet"]);
    assert!(path_output.status.success());
    let path = String::from_utf8_lossy(&path_output.stdout).trim().to_string();
    assert!(
        PathBuf::from(&path).exists(),
        "path returned should exist: {path}"
    );
}

#[test]
fn completions_generates_zsh() {
    let output = Command::new(git_waku_bin())
        .args(["completions", "zsh"])
        .output()
        .expect("failed to run git-waku completions");
    assert!(
        output.status.success(),
        "git-waku completions zsh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "completions should produce output"
    );
    assert!(
        stdout.contains("git-waku") || stdout.contains("_git-waku"),
        "completions should reference the binary name: {stdout}"
    );
}

#[test]
fn remove_dirty_check_ignores_waku_symlinks() {
    let (_tmp, repo) = setup_repo();

    // Configure a symlink entry
    run_git(&repo, &["config", "waku.link.include", "node_modules"]);
    fs::create_dir(repo.join("node_modules")).unwrap();
    fs::write(repo.join("node_modules/pkg.json"), "{}").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "add node_modules"]);

    run_waku(&repo, &["create", "feature-waku-link"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-waku-link");

    // The symlink itself is a waku artifact and should not count as dirty.
    // Remove without --force should succeed on a clean worktree with only waku artifacts.
    let output = run_waku(&repo, &["remove", "feature-waku-link"]);
    assert!(
        output.status.success(),
        "remove should succeed when only waku artifacts are present: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!wt_path.exists());
}

#[test]
fn remove_dirty_check_detects_real_changes_alongside_waku_artifacts() {
    let (_tmp, repo) = setup_repo();

    run_git(&repo, &["config", "waku.link.include", "node_modules"]);
    fs::create_dir(repo.join("node_modules")).unwrap();
    fs::write(repo.join("node_modules/pkg.json"), "{}").unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "add node_modules"]);

    run_waku(&repo, &["create", "feature-waku-dirty"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-waku-dirty");

    // Add a real untracked file (not a waku artifact)
    fs::write(wt_path.join("real-change.txt"), "dirty\n").unwrap();

    let output = run_waku(&repo, &["remove", "feature-waku-dirty"]);
    assert!(
        !output.status.success(),
        "remove should fail when real changes exist alongside waku artifacts"
    );
    assert!(wt_path.exists(), "worktree should not be removed");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--force"),
        "should suggest --force: {stderr}"
    );
}

#[test]
fn remove_cleans_up_empty_dirs_even_when_branch_delete_fails() {
    let (_tmp, repo) = setup_repo();

    run_waku(&repo, &["create", "feature-branch-fail"]);
    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-branch-fail");

    // Add an unmerged commit so that `git branch -d` (without -D) will fail
    fs::write(wt_path.join("unmerged.txt"), "not merged\n").unwrap();
    run_git(&wt_path, &["add", "."]);
    run_git(&wt_path, &["commit", "-m", "unmerged commit"]);

    // Remove without --force: worktree is clean, but branch is not fully merged
    let output = run_waku(&repo, &["remove", "feature-branch-fail"]);
    assert!(
        output.status.success(),
        "remove should succeed even when branch delete fails: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!wt_path.exists(), "worktree should be removed");

    // Branch delete warning should appear
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning"),
        "should warn about failed branch delete: {stderr}"
    );

    // Empty worktrees directory should still be cleaned up
    let worktrees_dir = repo.parent().unwrap().join("myrepo-worktrees");
    assert!(
        !worktrees_dir.exists(),
        "empty worktrees directory should be cleaned up"
    );
}

#[test]
fn config_get_regexp_returns_empty_for_no_match() {
    let (_tmp, repo) = setup_repo();

    // No waku config set — should return empty without error
    let output = run_waku(&repo, &["create", "feature-no-config"]);
    assert!(
        output.status.success(),
        "create should succeed without any waku config: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-no-config");
    assert!(wt_path.exists());
}

#[test]
fn create_worktreeinclude_glob_copies_nested_env() {
    let (_tmp, repo) = setup_repo();

    // Create a nested .env file that is gitignored
    fs::create_dir(repo.join("sub")).unwrap();
    fs::write(repo.join("sub/.env"), "SUB_SECRET=abc").unwrap();
    fs::write(repo.join(".env"), "ROOT_SECRET=xyz").unwrap();

    fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    run_git(&repo, &["add", ".gitignore"]);
    run_git(&repo, &["commit", "-m", "add gitignore"]);

    // Use glob pattern in .worktreeinclude
    fs::write(repo.join(".worktreeinclude"), "**/.env\n").unwrap();

    let output = run_waku(&repo, &["create", "feature-glob-env"]);
    assert!(
        output.status.success(),
        "git-waku create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/feature-glob-env");
    assert!(
        wt_path.join(".env").exists(),
        "root .env should be copied to worktree"
    );
    assert!(
        wt_path.join("sub/.env").exists(),
        "sub/.env should be copied to worktree via glob pattern"
    );
    assert_eq!(
        fs::read_to_string(wt_path.join("sub/.env")).unwrap(),
        "SUB_SECRET=abc"
    );
}

#[test]
fn clean_dry_run_dirty_and_unchanged() {
    let (_tmp, repo) = setup_repo();

    // Create an undiverged worktree (no commits) and make it dirty
    run_waku(&repo, &["create", "feature-dirty-unchanged"]);
    let wt_path = repo
        .parent()
        .unwrap()
        .join("myrepo-worktrees/feature-dirty-unchanged");
    fs::write(wt_path.join("scratch.txt"), "wip\n").unwrap();

    let output = run_waku(&repo, &["clean", "--dry-run"]);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("dirty"),
        "should show dirty label: {stdout}"
    );
    assert!(
        stdout.contains("no changes"),
        "should also show no changes when dirty and unchanged: {stdout}"
    );
}

#[test]
fn create_with_existing_branch() {
    let (_tmp, repo) = setup_repo();

    // Create a branch without a worktree
    run_git(&repo, &["branch", "existing-branch"]);

    // Creating a worktree for the existing branch should succeed
    let output = run_waku(&repo, &["create", "existing-branch"]);
    assert!(
        output.status.success(),
        "create with existing branch should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let wt_path = repo.parent().unwrap().join("myrepo-worktrees/existing-branch");
    assert!(wt_path.exists(), "worktree directory should exist");
}

#[test]
fn create_with_existing_branch_ignores_from() {
    let (_tmp, repo) = setup_repo();

    // Create a branch without a worktree
    run_git(&repo, &["branch", "existing-from"]);

    // --from should be ignored for existing branches (with a warning)
    let output = run_waku(&repo, &["create", "existing-from", "--from", "HEAD"]);
    assert!(
        output.status.success(),
        "create with existing branch and --from should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--from"),
        "should warn about --from being ignored: {stderr}"
    );
}
