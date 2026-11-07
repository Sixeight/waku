use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn setup_repo() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
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
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {} failed: status {:?} stdout {} stderr {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_waku_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_git-waku"))
}

fn run_waku(dir: &Path, args: &[&str]) -> Output {
    Command::new(git_waku_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git-waku")
}

fn output_within(dir: &Path, args: &[&str], timeout: Duration) -> Output {
    let mut child = Command::new(git_waku_bin())
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git-waku");
    let started = Instant::now();
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_end(&mut stdout)
                    .unwrap();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_end(&mut stderr)
                    .unwrap();
                return Output {
                    status,
                    stdout,
                    stderr,
                };
            }
            None if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "git-waku {} did not exit within {timeout:?}",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn add_worktree(repo: &Path, branch: &str, path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    run_git(
        repo,
        &["worktree", "add", "-b", branch, path.to_str().unwrap()],
    );
}

fn merged_worktree(repo: &Path, branch: &str) -> PathBuf {
    let path = repo
        .parent()
        .unwrap()
        .join(format!("myrepo-worktrees/{branch}"));
    add_worktree(repo, branch, &path);
    fs::write(path.join(format!("{branch}.txt")), format!("{branch}\n")).unwrap();
    run_git(&path, &["add", "."]);
    run_git(&path, &["commit", "-m", "add feature"]);
    run_git(repo, &["merge", "--no-ff", branch, "-m", "merge"]);
    path
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

fn worktree_paths(repo: &Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(str::to_string)
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn duplicate_directory_names_fail_without_deleting_or_creating() {
    let (tmp, repo) = setup_repo();
    let first = tmp.path().join("dir-a/same");
    let second = tmp.path().join("dir-b/same");
    add_worktree(&repo, "branch-a", &first);
    add_worktree(&repo, "branch-b", &second);
    let before = worktree_paths(&repo);

    for args in [
        vec!["path", "same"],
        vec!["remove", "same"],
        vec!["open", "same", "--editor=true"],
    ] {
        let output = output_within(&repo, &args, Duration::from_secs(5));
        assert_eq!(
            output.status.code(),
            Some(1),
            "{args:?}\n{}",
            text(&output.stderr)
        );
        let stderr = text(&output.stderr);
        assert!(
            stderr.contains("multiple worktrees match 'same'"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains(&first.canonicalize().unwrap().display().to_string()),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains(&second.canonicalize().unwrap().display().to_string()),
            "{args:?}: {stderr}"
        );
        assert!(
            text(&output.stdout).trim().is_empty(),
            "{args:?} stdout: {}",
            text(&output.stdout)
        );
        assert_eq!(worktree_paths(&repo), before, "{args:?} changed worktrees");
        assert!(first.join("README.md").exists());
        assert!(second.join("README.md").exists());
        assert!(branch_exists(&repo, "branch-a"));
        assert!(branch_exists(&repo, "branch-b"));
    }
}

#[test]
fn exact_branch_name_beats_duplicate_directory_names() {
    let (tmp, repo) = setup_repo();
    let first = tmp.path().join("dir-a/same");
    let second = tmp.path().join("dir-b/same");
    let literal = tmp.path().join("literal");
    add_worktree(&repo, "branch-a", &first);
    add_worktree(&repo, "branch-b", &second);
    add_worktree(&repo, "same", &literal);

    let output = run_waku(&repo, &["path", "same"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let printed = PathBuf::from(text(&output.stdout).trim());
    let head = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&printed)
        .output()
        .unwrap();
    assert!(head.status.success(), "{}", text(&head.stderr));
    assert_eq!(text(&head.stdout).trim(), "same");
    assert!(first.exists());
    assert!(second.exists());

    let removed = run_waku(&repo, &["remove", "same"]);
    assert!(removed.status.success(), "{}", text(&removed.stderr));
    assert!(!printed.exists());
    assert!(first.join("README.md").exists());
    assert!(second.join("README.md").exists());
    assert!(!branch_exists(&repo, "same"));
    assert!(branch_exists(&repo, "branch-a"));
    assert!(branch_exists(&repo, "branch-b"));
}

#[test]
fn clean_without_a_terminal_fails_before_hiding_the_cursor() {
    let (_tmp, repo) = setup_repo();
    let path = repo.parent().unwrap().join("myrepo-worktrees/feature-hang");
    add_worktree(&repo, "feature-hang", &path);

    let output = output_within(&repo, &["clean"], Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains(
            "clean needs a terminal to choose worktrees; pass --yes to remove the default selection, or --dry-run to preview it"
        ),
        "{stderr}"
    );
    assert!(
        !output
            .stderr
            .windows(b"\x1b[?25l".len())
            .any(|w| w == b"\x1b[?25l"),
        "selector hid the cursor without a terminal: {stderr:?}"
    );
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    assert!(path.join("README.md").exists());
    assert!(branch_exists(&repo, "feature-hang"));

    let preview = run_waku(&repo, &["clean", "--dry-run"]);
    assert!(preview.status.success(), "{}", text(&preview.stderr));
    assert!(text(&preview.stdout).contains("feature-hang"));
    assert!(path.join("README.md").exists());
}

#[test]
fn clean_with_nothing_to_remove_still_exits_on_a_pipe() {
    let (_tmp, repo) = setup_repo();
    let output = output_within(&repo, &["clean"], Duration::from_secs(5));
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("No worktrees to clean."));
}

#[test]
fn clean_dry_run_keeps_long_branch_names_when_stdout_is_piped() {
    let (_tmp, repo) = setup_repo();
    let branch = format!("feature-{}", "n".repeat(80));
    let path = merged_worktree(&repo, &branch);

    let output = run_waku(&repo, &["clean", "--dry-run"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains(&branch), "{stdout}");
    assert!(!stdout.contains('…'), "{stdout}");
    assert!(path.join(format!("{branch}.txt")).exists());
}

#[cfg(unix)]
#[test]
fn clean_reports_partial_deletion_failure_from_git() {
    let (tmp, repo) = setup_repo();
    let kept = merged_worktree(&repo, "keep-merged");
    let failed = merged_worktree(&repo, "fail-remove");
    let shim_dir = tmp.path().join("shim");
    fs::create_dir(&shim_dir).unwrap();
    let original_path = std::env::var_os("PATH").expect("PATH");
    let real_git = std::env::split_paths(&original_path)
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file())
        .expect("git on PATH");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = worktree ] && [ \"$2\" = remove ]; then\n  for arg in \"$@\"; do\n    case \"$arg\" in\n      *fail-remove*)\n        echo \"shim refused worktree remove\" >&2\n        exit 1\n        ;;\n    esac\n  done\nfi\nexec '{}' \"$@\"\n",
        real_git.display().to_string().replace('\'', "'\\''")
    );
    let shim = shim_dir.join("git");
    fs::write(&shim, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(shim_dir).chain(std::env::split_paths(&original_path)),
    )
    .unwrap();

    let output = Command::new(git_waku_bin())
        .args(["clean", "--yes"])
        .current_dir(&repo)
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("Removed 1 of 2. 1 failed."), "{stderr}");
    assert!(stderr.contains("shim refused worktree remove"), "{stderr}");
    assert!(!kept.exists(), "successful deletion should stay deleted");
    assert!(!branch_exists(&repo, "keep-merged"));
    assert!(
        failed.join("fail-remove.txt").exists(),
        "failed worktree should keep its files"
    );
    assert!(branch_exists(&repo, "fail-remove"));
    assert!(
        worktree_paths(&repo)
            .iter()
            .any(|listed| listed.contains("fail-remove")),
        "failed worktree should stay registered"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn clean_reports_a_permission_denied_deletion() {
    if fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("Uid:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u32>()
                    .ok()
            })
        })
        == Some(0)
    {
        return;
    }

    let (_tmp, repo) = setup_repo();
    let path = merged_worktree(&repo, "only-merged");
    let parent = path.parent().unwrap().to_path_buf();
    let _restore = RestoreMode(parent.clone());
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();

    let output = run_waku(&repo, &["clean", "--yes"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("Removed 0 of 1. 1 failed."), "{stderr}");
    assert!(
        stderr.to_lowercase().contains("permission denied"),
        "{stderr}"
    );
    assert!(
        path.exists(),
        "permission failure should leave the worktree directory"
    );
    assert!(
        branch_exists(&repo, "only-merged"),
        "a failed worktree removal must not delete the branch"
    );
}

#[cfg(target_os = "linux")]
struct RestoreMode(PathBuf);

#[cfg(target_os = "linux")]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod pty {
    use super::*;
    use std::ffi::{c_char, CStr};
    use std::io::ErrorKind;
    use std::os::fd::{FromRawFd, OwnedFd};

    #[repr(C)]
    struct Winsize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }

    #[cfg(target_os = "linux")]
    const TIOCSWINSZ: u64 = 0x5414;
    #[cfg(target_os = "macos")]
    const TIOCSWINSZ: u64 = 0x80087467;
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 4;
    #[cfg(target_os = "linux")]
    const O_NONBLOCK: i32 = 0o4000;
    #[cfg(target_os = "macos")]
    const O_NONBLOCK: i32 = 0x4;

    #[link(name = "util")]
    extern "C" {
        fn openpty(
            amaster: *mut i32,
            aslave: *mut i32,
            name: *mut c_char,
            termp: *const u8,
            winp: *const Winsize,
        ) -> i32;
    }

    extern "C" {
        fn fcntl(fd: i32, cmd: i32, arg: i32) -> i32;
        fn ioctl(fd: i32, request: u64, arg: *const Winsize) -> i32;
    }

    struct Pty {
        master: File,
        slave: OwnedFd,
        name: String,
    }

    fn open_pty(rows: u16, cols: u16) -> Pty {
        let mut master = -1;
        let mut slave = -1;
        let mut name: [c_char; 256] = [0; 256];
        let win = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe {
            openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null(),
                &win,
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
        unsafe {
            assert_eq!(ioctl(slave, TIOCSWINSZ, &win), 0);
            let flags = fcntl(master, F_GETFL, 0);
            assert!(flags >= 0, "fcntl: {}", std::io::Error::last_os_error());
            assert_eq!(fcntl(master, F_SETFL, flags | O_NONBLOCK), 0);
        }
        let name = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let rows_s = rows.to_string();
        let cols_s = cols.to_string();
        #[cfg(target_os = "linux")]
        let device_flag = "-F";
        #[cfg(target_os = "macos")]
        let device_flag = "-f";
        let status = Command::new("stty")
            .args([
                device_flag,
                &name,
                "rows",
                &rows_s,
                "cols",
                &cols_s,
                "-echo",
                "-isig",
                "-icanon",
                "min",
                "1",
                "time",
                "0",
            ])
            .status()
            .expect("stty");
        assert!(status.success(), "stty failed to configure {name}");
        Pty {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { OwnedFd::from_raw_fd(slave) },
            name,
        }
    }

    fn read_available(master: &mut File, buf: &mut Vec<u8>) {
        let mut tmp = [0u8; 4096];
        loop {
            match master.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::Interrupted =>
                {
                    break;
                }
                Err(error) => panic!("read pty: {error}"),
            }
        }
    }

    fn drive(
        dir: &Path,
        args: &[&str],
        cols: u16,
        key: Option<&[u8]>,
    ) -> (ExitStatus, Vec<u8>, Vec<u8>) {
        let mut pty = open_pty(24, cols);
        let stdout_path = dir.join(".waku-stdout");
        let stderr_is_pty = key.is_some();
        let mut child = Command::new(git_waku_bin());
        child.args(args).current_dir(dir);
        if stderr_is_pty {
            let stdin = pty.slave.try_clone().unwrap();
            let stderr = pty.slave.try_clone().unwrap();
            child
                .stdin(Stdio::from(stdin))
                .stdout(File::create(&stdout_path).unwrap())
                .stderr(Stdio::from(stderr));
        } else {
            child
                .stdin(Stdio::null())
                .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
                .stderr(File::create(&stdout_path).unwrap());
        }
        drop(pty.slave);
        let mut child = child.spawn().expect("spawn git-waku");
        let started = Instant::now();
        let mut captured = Vec::new();
        let mut sent = key.is_none();
        let mut resent = false;
        let mut sent_at = None;
        let status = loop {
            read_available(&mut pty.master, &mut captured);
            if !sent
                && captured
                    .windows(b"selected".len())
                    .any(|window| window == b"selected")
            {
                pty.master.write_all(key.unwrap()).unwrap();
                sent = true;
                sent_at = Some(Instant::now());
            } else if sent
                && !resent
                && sent_at.is_some_and(|at| at.elapsed() > Duration::from_millis(400))
            {
                pty.master.write_all(key.unwrap()).unwrap();
                resent = true;
            }
            if let Some(status) = child.try_wait().unwrap() {
                read_available(&mut pty.master, &mut captured);
                break status;
            }
            if started.elapsed() > Duration::from_secs(8) {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "git-waku {} did not exit; pty {} captured:\n{}",
                    args.join(" "),
                    pty.name,
                    text(&captured)
                );
            }
            std::thread::sleep(Duration::from_millis(15));
        };
        let piped = fs::read(&stdout_path).unwrap_or_default();
        if stderr_is_pty {
            (status, piped, captured)
        } else {
            (status, captured, piped)
        }
    }

    #[test]
    fn stdout_pipe_still_prompts_on_a_terminal() {
        let (_tmp, repo) = setup_repo();
        let path = repo
            .parent()
            .unwrap()
            .join("myrepo-worktrees/feature-prompt");
        add_worktree(&repo, "feature-prompt", &path);

        let (status, stdout, stderr) = drive(&repo, &["clean"], 80, Some(b"q"));
        assert!(status.success(), "{}", text(&stderr));
        assert!(text(&stdout).contains("Aborted."), "{}", text(&stdout));
        assert!(
            !text(&stderr).contains("needs a terminal"),
            "{}",
            text(&stderr)
        );
        assert!(path.join("README.md").exists());
        assert!(branch_exists(&repo, "feature-prompt"));
    }

    #[test]
    fn ctrl_c_restores_the_cursor_and_keeps_the_worktree() {
        let (_tmp, repo) = setup_repo();
        let path = repo
            .parent()
            .unwrap()
            .join("myrepo-worktrees/feature-cancel");
        add_worktree(&repo, "feature-cancel", &path);

        let (status, stdout, stderr) = drive(&repo, &["clean"], 80, Some(b"\x03"));
        assert_eq!(status.code(), Some(130), "{}", text(&stderr));
        assert!(
            text(&stderr).contains("cancelled; no selected deletions were applied"),
            "{}",
            text(&stderr)
        );
        assert!(!text(&stdout).contains("Aborted."), "{}", text(&stdout));
        let hide = last_pos(&stderr, b"\x1b[?25l");
        let show = last_pos(&stderr, b"\x1b[?25h");
        assert!(
            matches!((hide, show), (Some(hide_at), Some(show_at)) if show_at > hide_at),
            "cursor was not restored after it was hidden: {stderr:?}"
        );
        assert!(path.join("README.md").exists());
        assert!(branch_exists(&repo, "feature-cancel"));
    }

    #[test]
    fn dry_run_on_a_narrow_terminal_still_truncates() {
        let (_tmp, repo) = setup_repo();
        let branch = "feature-tty";
        let path = merged_worktree(&repo, branch);

        let (status, stdout, stderr) = drive(&repo, &["clean", "--dry-run"], 40, None);
        assert!(status.success(), "{}", text(&stderr));
        let stdout = text(&stdout);
        assert!(!stdout.contains(branch), "{stdout}");
        assert!(stdout.contains('…'), "{stdout}");
        assert!(path.join(format!("{branch}.txt")).exists());
    }

    fn last_pos(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .rposition(|window| window == needle)
    }
}
