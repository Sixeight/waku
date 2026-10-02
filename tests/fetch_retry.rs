use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn command(program: &str, dir: &Path, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = command("git", dir, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

struct FetchFixture {
    _tmp: TempDir,
    repo: PathBuf,
    initial: String,
    target: String,
}

impl FetchFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let upstream = tmp.path().join("upstream");
        fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["config", "user.email", "test@example.com"]);
        fs::write(upstream.join("content.txt"), "initial\n").unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-qm", "initial"]);
        git(&upstream, &["branch", "stale"]);
        let initial = git(&upstream, &["rev-parse", "HEAD"]);

        let repo = tmp.path().join("repo");
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                upstream.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
        fs::write(upstream.join("content.txt"), "latest\n").unwrap();
        git(&upstream, &["commit", "-qam", "advance main"]);
        let target = git(&upstream, &["rev-parse", "HEAD"]);
        git(&repo, &["fetch", "--refmap=", "origin", "main"]);
        git(&upstream, &["branch", "-D", "stale"]);

        Self {
            _tmp: tmp,
            repo,
            initial,
            target,
        }
    }

    fn install_ref_race(&self) {
        let hook = self.repo.join(".git/hooks/reference-transaction");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\n\
                 set -eu\n\
                 [ \"$1\" = prepared ] || exit 0\n\
                 while read -r before after ref; do\n\
                 [ \"$ref\" = refs/remotes/origin/stale ] || continue\n\
                 git update-ref refs/remotes/origin/main {}\n\
                 echo race >> \"${{0%/*}}/fetch-races\"\n\
                 done\n",
                self.target
            ),
        )
        .unwrap();
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn race_count(&self) -> usize {
        fs::read_to_string(self.repo.join(".git/hooks/fetch-races"))
            .unwrap()
            .lines()
            .count()
    }

    fn create_with_fetch_error(&self, message: &str) -> (Output, usize) {
        let upload_pack = self.repo.parent().unwrap().join("upload-pack");
        fs::write(
            &upload_pack,
            format!(
                "#!/bin/sh\n\
                 echo attempt >> \"$0.attempts\"\n\
                 printf '%s\\n' '{}' >&2\n\
                 exit 1\n",
                message.replace('\'', "'\\''")
            ),
        )
        .unwrap();
        fs::set_permissions(&upload_pack, fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &self.repo,
            &[
                "config",
                "remote.origin.uploadpack",
                upload_pack.to_str().unwrap(),
            ],
        );
        let output = command(
            env!("CARGO_BIN_EXE_git-waku"),
            &self.repo,
            &["create", "feature", "--fetch", "--from-default-branch"],
        );
        let attempts = fs::read_to_string(upload_pack.with_extension("attempts"))
            .unwrap()
            .lines()
            .count();
        (output, attempts)
    }

    fn worktree_path(&self, branch: &str) -> PathBuf {
        self.repo
            .parent()
            .unwrap()
            .join("repo-worktrees")
            .join(branch)
    }
}

#[test]
fn create_retries_ref_race_from_another_worktree() {
    let fixture = FetchFixture::new();
    let existing = fixture.repo.parent().unwrap().join("existing");
    git(
        &fixture.repo,
        &[
            "worktree",
            "add",
            "-qb",
            "existing",
            existing.to_str().unwrap(),
        ],
    );
    fixture.install_ref_race();

    let output = command(
        env!("CARGO_BIN_EXE_git-waku"),
        &existing,
        &["create", "feature", "--fetch", "--from-default-branch"],
    );
    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.race_count() > 0);
    let worktree = fixture.worktree_path("feature");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), fixture.target);
    assert_eq!(
        fs::read_to_string(worktree.join("content.txt")).unwrap(),
        "latest\n"
    );
    assert_eq!(git(&existing, &["rev-parse", "HEAD"]), fixture.initial);
}

#[test]
fn background_fetch_retries_ref_race() {
    let fixture = FetchFixture::new();
    fixture.install_ref_race();
    let output = command(
        env!("CARGO_BIN_EXE_git-waku"),
        &fixture.repo,
        &[
            "__background-fetch",
            fixture.repo.to_str().unwrap(),
            fixture.repo.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "background fetch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.race_count() > 0);
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/main"]),
        fixture.target
    );
    assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), fixture.initial);
}

#[test]
fn create_stops_after_repeated_ref_races() {
    let fixture = FetchFixture::new();
    let (output, attempts) = fixture.create_with_fetch_error(&format!(
        "error: cannot lock ref 'refs/remotes/origin/main': is at {} but expected {}",
        fixture.target, fixture.initial
    ));
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot lock ref 'refs/remotes/origin/main': is at "),
        "{stderr}"
    );
    assert_eq!(attempts, 3);
    assert!(!fixture.worktree_path("feature").exists());
    assert!(!command(
        "git",
        &fixture.repo,
        &["show-ref", "--verify", "refs/heads/feature"]
    )
    .status
    .success());
}

#[test]
fn create_does_not_retry_authentication_failure() {
    let fixture = FetchFixture::new();
    let (output, attempts) = fixture.create_with_fetch_error("fatal: Authentication failed");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Authentication failed"));
    assert_eq!(attempts, 1);
    assert!(!fixture.worktree_path("feature").exists());
}

#[test]
fn create_preserves_ref_lock_on_fetch_failure() {
    let fixture = FetchFixture::new();
    let lock = fixture.repo.join(".git/refs/remotes/origin/main.lock");
    fs::write(&lock, "held by another process\n").unwrap();
    let output = command(
        env!("CARGO_BIN_EXE_git-waku"),
        &fixture.repo,
        &["create", "feature", "--fetch", "--from-default-branch"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("File exists"), "{stderr}");
    assert_eq!(
        fs::read_to_string(lock).unwrap(),
        "held by another process\n"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", "origin/main"]),
        fixture.initial
    );
    assert!(!fixture.worktree_path("feature").exists());
}
