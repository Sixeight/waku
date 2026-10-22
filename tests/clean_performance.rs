use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn command(program: &str, root: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = command("git", root, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

struct Fixture {
    temporary: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn supports_batch_counts(&self) -> bool {
        command(
            "git",
            &self.root,
            &[
                "for-each-ref",
                "--format=%(ahead-behind:HEAD)",
                "refs/heads/main",
            ],
        )
        .status
        .success()
    }

    fn new(diverged_targets: bool) -> Self {
        let temporary = TempDir::new().unwrap();
        let root = temporary.path().join("repo");
        let remote = temporary.path().join("origin.git");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.name", "Test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        git(&root, &["commit", "--allow-empty", "-qm", "initial"]);
        git(&root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
        git(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&root, &["push", "-qu", "origin", "main"]);
        if diverged_targets {
            git(&root, &["checkout", "-qb", "remote-main"]);
            fs::write(root.join("remote.txt"), "remote change\n").unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-qm", "remote"]);
            git(&root, &["push", "-q", "origin", "HEAD:main"]);
            git(&root, &["checkout", "-q", "main"]);
            fs::write(root.join("local.txt"), "local change\n").unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-qm", "local"]);
        }
        for index in 0..2 {
            let branch = format!("gone-{index}");
            let worktree = temporary.path().join(&branch);
            git(
                &root,
                &[
                    "worktree",
                    "add",
                    "-qb",
                    &branch,
                    worktree.to_str().unwrap(),
                ],
            );
            if diverged_targets {
                git(
                    &worktree,
                    &["merge", "--no-ff", "origin/main", "-qm", "merge targets"],
                );
            }
            fs::write(worktree.join("work.txt"), format!("unique work {index}\n")).unwrap();
            git(&worktree, &["add", "."]);
            git(&worktree, &["commit", "-qm", "work"]);
            git(
                &root,
                &["config", &format!("branch.{branch}.remote"), "origin"],
            );
            git(
                &root,
                &[
                    "config",
                    &format!("branch.{branch}.merge"),
                    &format!("refs/heads/{branch}"),
                ],
            );
        }
        Self { temporary, root }
    }

    fn clean(&self, shim: Option<&Path>) -> (String, String) {
        let trace = self.root.join(".git/clean.trace");
        let mut process = Command::new(env!("CARGO_BIN_EXE_git-waku"));
        process
            .args(["clean", "--dry-run"])
            .current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("NO_COLOR", "1")
            .env("COLUMNS", "240")
            .env("GIT_TRACE", &trace);
        if let Some(shim) = shim {
            let path = std::env::var_os("PATH").unwrap();
            let paths = std::iter::once(shim.to_path_buf()).chain(std::env::split_paths(&path));
            process.env("PATH", std::env::join_paths(paths).unwrap());
        }
        let output = process.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            String::from_utf8(output.stdout).unwrap(),
            fs::read_to_string(trace).unwrap(),
        )
    }
}

#[test]
fn clean_batches_unique_counts_for_a_shared_target() {
    let fixture = Fixture::new(false);
    let supported = fixture.supports_batch_counts();
    let (output, trace) = fixture.clean(None);
    assert_eq!(output.matches("1 unique").count(), 2, "{output}");
    assert!(trace.contains("ahead-behind:"), "{trace}");
    assert_eq!(
        trace.matches("rev-list --count HEAD --not").count(),
        if supported { 0 } else { 2 },
        "{trace}"
    );
}

#[test]
fn clean_preserves_union_counts_when_targets_diverge() {
    let fixture = Fixture::new(true);
    let (output, trace) = fixture.clean(None);
    assert_eq!(output.matches("2 unique").count(), 2, "{output}");
    assert!(!trace.contains("ahead-behind:"), "{trace}");
    assert_eq!(
        trace.matches("rev-list --count HEAD --not").count(),
        2,
        "{trace}"
    );
}

#[test]
fn clean_counts_detached_heads_that_are_not_in_the_batch() {
    let fixture = Fixture::new(false);
    let supported = fixture.supports_batch_counts();
    let detached = fixture.temporary.path().join("detached");
    git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--detach",
            detached.to_str().unwrap(),
            "main",
        ],
    );
    fs::write(detached.join("detached.txt"), "detached work\n").unwrap();
    git(&detached, &["add", "."]);
    git(&detached, &["commit", "-qm", "detached"]);
    let (output, trace) = fixture.clean(None);
    assert_eq!(output.matches("1 unique").count(), 3, "{output}");
    assert!(trace.contains("ahead-behind:"), "{trace}");
    assert_eq!(
        trace.matches("rev-list --count HEAD --not").count(),
        if supported { 1 } else { 3 },
        "{trace}"
    );
}

#[test]
fn clean_falls_back_when_git_rejects_batch_counts() {
    let fixture = Fixture::new(false);
    let real_git = command("sh", &fixture.root, &["-c", "command -v git"]);
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let shim = fixture.temporary.path().join("bin");
    fs::create_dir(&shim).unwrap();
    let script = shim.join("git");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor arg do\ncase \"$arg\" in *ahead-behind:*) echo attempt >> \"$0.attempts\"; exit 128;; esac\ndone\nexec '{}' \"$@\"\n",
            real_git.trim().replace('\'', "'\\''")
        ),
    ).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let (output, trace) = fixture.clean(Some(&shim));
    assert_eq!(output.matches("1 unique").count(), 2, "{output}");
    assert_eq!(
        fs::read_to_string(shim.join("git.attempts")).unwrap(),
        "attempt\n"
    );
    assert_eq!(
        trace.matches("rev-list --count HEAD --not").count(),
        2,
        "{trace}"
    );
}

#[test]
fn clean_batches_squash_checks() {
    let fixture = Fixture::new(false);
    let (output, trace) = fixture.clean(None);
    assert_eq!(output.matches("1 unique").count(), 2, "{output}");
    assert!(trace.contains("merge-tree --write-tree --stdin"), "{trace}");
}

#[test]
fn clean_keeps_binary_conflicts_even_when_the_merge_tree_matches_main() {
    let fixture = Fixture::new(false);
    fs::write(fixture.root.join("work.txt"), b"main\0content\n").unwrap();
    git(&fixture.root, &["add", "."]);
    git(&fixture.root, &["commit", "-qm", "binary main"]);
    git(&fixture.root, &["push", "-q", "origin", "main"]);
    let merged = command(
        "git",
        &fixture.root,
        &["merge-tree", "--write-tree", "main", "gone-0"],
    );
    assert_eq!(merged.status.code(), Some(1));
    let tree = git(&fixture.root, &["rev-parse", "main^{tree}"]);
    assert_eq!(
        String::from_utf8_lossy(&merged.stdout).lines().next(),
        Some(tree.as_str())
    );
    let (output, _) = fixture.clean(None);
    assert_eq!(output.matches("closed").count(), 2, "{output}");
    assert_eq!(output.matches("1 unique").count(), 2, "{output}");
}

#[test]
fn clean_preserves_result_order_across_different_merge_targets() {
    let fixture = Fixture::new(false);
    let base = git(&fixture.root, &["rev-parse", "main"]);
    fs::write(fixture.root.join("work.txt"), "unique work 0\n").unwrap();
    git(&fixture.root, &["add", "."]);
    git(&fixture.root, &["commit", "-qm", "squash first"]);
    git(&fixture.root, &["checkout", "-qb", "remote-main", &base]);
    fs::write(fixture.root.join("work.txt"), "unique work 1\n").unwrap();
    git(&fixture.root, &["add", "."]);
    git(&fixture.root, &["commit", "-qm", "squash second"]);
    git(&fixture.root, &["push", "-q", "origin", "HEAD:main"]);
    git(&fixture.root, &["checkout", "-q", "main"]);
    let (output, _) = fixture.clean(None);
    let rows: Vec<_> = output
        .lines()
        .filter(|line| line.contains("gone-"))
        .collect();
    assert_eq!(rows.len(), 2, "{output}");
    assert!(rows.iter().all(|line| line.contains("merged")), "{output}");
}

#[test]
fn clean_falls_back_when_git_rejects_batch_merges() {
    let fixture = Fixture::new(false);
    let real_git = command("sh", &fixture.root, &["-c", "command -v git"]);
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let shim = fixture.temporary.path().join("bin");
    fs::create_dir(&shim).unwrap();
    let script = shim.join("git");
    fs::write(&script, format!(
        "#!/bin/sh\nif [ \"$1\" = merge-tree ]; then\nfor arg do\nif [ \"$arg\" = --stdin ]; then echo attempt >> \"$0.attempts\"; exit 128; fi\ndone\nfi\nexec '{}' \"$@\"\n",
        real_git.trim().replace('\'', "'\\''")
    )).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let (output, trace) = fixture.clean(Some(&shim));
    assert_eq!(output.matches("closed").count(), 2, "{output}");
    assert_eq!(output.matches("1 unique").count(), 2, "{output}");
    assert_eq!(
        fs::read_to_string(shim.join("git.attempts")).unwrap(),
        "attempt\n"
    );
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("built-in: git merge-tree --write-tree"))
            .count(),
        2,
        "{trace}"
    );
}
