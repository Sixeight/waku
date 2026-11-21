use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Result};

use super::{cleanup_empty_dirs, print_warning, spinner};
use crate::{config::Config, git, worktree};

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn run(query: &str, force: bool, keep_branch: bool) -> Result<()> {
    let root = worktree::repo_root()?;
    let waku_config = Config::load(&root)?;
    let (path, branch) = worktree::resolve_with_branch(&root, query, &waku_config)?;

    let path_str = path.to_string_lossy().to_string();
    if path == root || path.canonicalize().ok().as_deref() == root.canonicalize().ok().as_deref() {
        bail!("cannot remove the main worktree");
    }

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
    let staged = remove_worktree_staged(&root, &path)?;
    sp.finish_and_clear();
    eprintln!("  {} Removed worktree", console::style("✔").green(),);

    // Overlap parallel deletion of staged worktree contents with branch deletion.
    let branch_outcome = std::thread::scope(|scope| {
        let delete_handle = staged
            .as_deref()
            .map(|dir| scope.spawn(move || remove_dir_all_parallel(dir)));
        let branch_res = if !keep_branch {
            branch.as_ref().map(|branch| {
                let delete_flag = if force { "-D" } else { "-d" };
                (
                    branch.as_str(),
                    git::git_output_in(&root, &["branch", delete_flag, branch]),
                )
            })
        } else {
            None
        };
        if let Some(handle) = delete_handle {
            handle.join().unwrap();
        }
        branch_res
    });

    if let Some((branch, result)) = branch_outcome {
        match result {
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

struct StagedContents {
    dir: PathBuf,
    moved: Vec<OsString>,
}

impl StagedContents {
    fn rollback(self, wt_path: &Path) {
        let _ = fs::create_dir_all(wt_path);
        for name in self.moved {
            let _ = fs::rename(self.dir.join(&name), wt_path.join(&name));
        }
        let _ = fs::remove_dir(&self.dir);
    }
}

/// Remove a worktree via `git worktree remove --force`. For non-trivial trees
/// without submodules, non-`.git` entries are atomically moved into a sibling
/// staging directory first so Git finishes immediately and callers can delete
/// the staged files in parallel. If `git worktree remove` fails, all staged
/// entries are rolled back intact.
pub(crate) fn remove_worktree_staged(root: &Path, wt_path: &Path) -> Result<Option<PathBuf>> {
    let path_str = wt_path.to_string_lossy();
    // Preserve Git's submodule refusal semantics when `.gitmodules` is present.
    if wt_path.join(".gitmodules").exists() {
        git::git_in(root, &["worktree", "remove", "--force", &path_str])?;
        return Ok(None);
    }

    let staged = stage_worktree_contents(wt_path);
    match git::git_in(root, &["worktree", "remove", "--force", &path_str]) {
        Ok(()) => Ok(staged.map(|s| s.dir)),
        Err(err) => {
            if let Some(staged) = staged {
                staged.rollback(wt_path);
            }
            Err(err)
        }
    }
}

fn stage_worktree_contents(wt_path: &Path) -> Option<StagedContents> {
    let parent = wt_path.parent()?;
    let mut entries = Vec::new();
    let mut has_subdir = false;
    for entry in fs::read_dir(wt_path).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        if entry.file_type().ok().is_some_and(|ft| ft.is_dir()) {
            has_subdir = true;
        }
        entries.push(name);
    }
    if !has_subdir && entries.len() < 16 {
        return None;
    }

    let dir = create_staging_dir(parent)?;
    let mut moved = Vec::with_capacity(entries.len());
    for name in entries {
        if fs::rename(wt_path.join(&name), dir.join(&name)).is_err() {
            let staged = StagedContents { dir, moved };
            staged.rollback(wt_path);
            return None;
        }
        moved.push(name);
    }
    Some(StagedContents { dir, moved })
}

fn create_staging_dir(parent: &Path) -> Option<PathBuf> {
    let pid = std::process::id();
    for _ in 0..32 {
        let seq = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".waku-removing-{pid}-{seq}"));
        match fs::create_dir(&candidate) {
            Ok(()) => return Some(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Expand a staged directory into child paths suitable for parallel removal.
pub(crate) fn expand_removal_tasks(dir: &Path) -> Vec<PathBuf> {
    let mut tasks = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return tasks;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().ok().is_some_and(|ft| ft.is_dir()) {
            expand_subdir_for_removal(path, &mut tasks);
        } else {
            tasks.push(path);
        }
    }
    tasks
}

fn expand_subdir_for_removal(mut dir: PathBuf, tasks: &mut Vec<PathBuf>) {
    loop {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            tasks.push(dir);
            return;
        };
        let children: Vec<_> = read_dir.flatten().collect();
        if children.is_empty() {
            tasks.push(dir);
            return;
        }
        if let [only] = children.as_slice() {
            if only.file_type().ok().is_some_and(|ft| ft.is_dir()) {
                dir = only.path();
                continue;
            }
        }
        for child in children {
            tasks.push(child.path());
        }
        return;
    }
}

pub(crate) fn remove_path_best_effort(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

pub(crate) fn remove_dir_all_parallel(dir: &Path) {
    let tasks = expand_removal_tasks(dir);
    if !tasks.is_empty() {
        crate::parallel::map_with_limit(&tasks, 4, |path| remove_path_best_effort(path));
    }
    let _ = fs::remove_dir_all(dir);
}

/// Check if a worktree has real modifications, ignoring waku artifacts.
/// Returns `true` (dirty) when git commands fail, to avoid accidental data loss.
pub fn is_worktree_dirty(path: &Path, config: &Config) -> bool {
    let waku_entries: HashSet<&str> = config
        .values("waku.link.include")
        .into_iter()
        .chain(config.values("waku.copy.include"))
        .collect();
    let waku_prefixes: Vec<String> = waku_entries.iter().map(|e| format!("{e}/")).collect();

    let is_waku_artifact = |line: &str| -> bool {
        waku_entries.contains(line) || waku_prefixes.iter().any(|p| line.starts_with(p.as_str()))
    };

    // Git's pathspec environment can turn exclusions into inclusions.
    let pathspec_defaults = [
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_NOGLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ]
    .iter()
    .all(|variable| std::env::var_os(variable).is_none());
    let literal_paths = pathspec_defaults
        && waku_entries.iter().all(|entry| {
            entry
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..")
        });
    let exclusions: Vec<_> = if literal_paths {
        waku_entries
            .iter()
            .map(|entry| format!(":(top,literal,exclude){entry}"))
            .collect()
    } else {
        Vec::new()
    };
    let mut args = vec![
        "--no-optional-locks",
        "status",
        "--porcelain=v1",
        "-z",
        if literal_paths {
            "--untracked-files=normal"
        } else {
            "--untracked-files=all"
        },
        "--ignore-submodules=none",
        "--no-renames",
    ];
    if !exclusions.is_empty() {
        args.push("--");
        args.extend(exclusions.iter().map(String::as_str));
    }
    let status = match git::git_output_raw_in(path, &args) {
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
    use tempfile::TempDir;

    #[test]
    fn status_parse_errors_are_dirty_even_when_artifacts_are_excluded() {
        for status in [b"?? \xff\0".as_slice(), b"?\0", b"??\0", b"XY file\0"] {
            assert!(status_is_dirty(status, |_| true), "{status:?}");
        }
    }

    #[test]
    fn remove_worktree_staged_rolls_back_on_locked_worktree_failure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("repo");
        fs::create_dir(&root).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            git::git_in(&root, &args).unwrap();
        }
        let wt = tmp.path().join("wt-locked");
        git::git_in(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "locked",
                wt.to_str().unwrap(),
            ],
        )
        .unwrap();
        // Add a nested directory so staging is triggered, then lock the worktree so
        // `git worktree remove --force` (single --force) fails.
        fs::create_dir_all(wt.join("node_modules/pkg")).unwrap();
        fs::write(wt.join("node_modules/pkg/index.js"), "keep me").unwrap();
        fs::write(wt.join("top.txt"), "top").unwrap();
        git::git_in(&root, &["worktree", "lock", wt.to_str().unwrap()]).unwrap();

        assert!(remove_worktree_staged(&root, &wt).is_err());
        assert_eq!(
            fs::read_to_string(wt.join("node_modules/pkg/index.js")).unwrap(),
            "keep me"
        );
        assert_eq!(fs::read_to_string(wt.join("top.txt")).unwrap(), "top");
        // No leftover `.waku-removing-*` directory in parent.
        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".waku-removing-")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover staging dirs: {leftovers:?}");
    }
}
