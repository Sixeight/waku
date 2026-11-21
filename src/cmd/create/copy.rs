use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{config::Config, git};

/// Remove an existing file, symlink, or directory at `path`.
/// No-op if the path does not exist.
pub(super) fn remove_existing(path: &Path) -> Result<()> {
    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove existing: {}", path.display()))?;
    } else if path.exists() || path.is_symlink() {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove existing: {}", path.display()))?;
    }
    Ok(())
}

/// The mode for handling `.worktreeinclude` entries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum WorktreeIncludeMode {
    Copy,
    Link,
    Ignore,
}

impl WorktreeIncludeMode {
    pub(super) fn from_config(waku_config: &Config) -> Self {
        match waku_config.last("waku.worktreeinclude") {
            Some("link") => Self::Link,
            Some("ignore") => Self::Ignore,
            _ => Self::Copy,
        }
    }
}

/// Collect paths matching `.worktreeinclude` that are also gitignored.
/// Returns relative paths from `root`.
pub(super) fn collect_worktreeinclude_files(root: &Path) -> Result<Vec<PathBuf>> {
    let Ok(wti_contents) = fs::read_to_string(root.join(".worktreeinclude")) else {
        return Ok(Vec::new());
    };

    // Combining exclude sources would select their union rather than their intersection.
    // `--directory` omits matches inside an untracked directory that is only partly matched,
    // so the file-level listing supplies the candidates.
    let (ignored, included, included_entries) = std::thread::scope(|scope| {
        let ignored = scope.spawn(|| git_ignored_paths(root, &["--exclude-standard"], true));
        let included =
            scope.spawn(|| git_ignored_paths(root, &["--exclude-from=.worktreeinclude"], true));
        let included_entries =
            scope.spawn(|| git_ignored_paths(root, &["--exclude-from=.worktreeinclude"], false));
        (
            ignored.join().unwrap(),
            included.join().unwrap(),
            included_entries.join().unwrap(),
        )
    });
    let ignored = ignored?;
    let included = included?;
    let (included_nested_repos, included_files): (Vec<_>, Vec<_>) = included_entries?
        .into_iter()
        .partition(|entry| entry.directory);
    if included_files.is_empty() {
        return Ok(Vec::new());
    }
    let included_files: Vec<PathBuf> = included_files.into_iter().map(|entry| entry.path).collect();
    let included_nested_repos: Vec<PathBuf> = included_nested_repos
        .into_iter()
        .map(|entry| entry.path)
        .collect();

    let mut saw_negation = false;
    let mut has_positive_pattern_after_negation = false;
    for line in wti_contents.lines() {
        if line.starts_with('!') {
            saw_negation = true;
        } else if saw_negation && !line.is_empty() && !line.starts_with('#') {
            has_positive_pattern_after_negation = true;
        }
    }

    let ignored_paths: HashSet<&Path> = ignored
        .iter()
        .filter(|entry| !entry.directory)
        .map(|entry| entry.path.as_path())
        .collect();
    let ignored_directories: HashSet<&Path> = ignored
        .iter()
        .filter(|entry| entry.directory)
        .map(|entry| entry.path.as_path())
        .collect();

    if included.iter().all(|entry| !entry.directory) && !has_positive_pattern_after_negation {
        let (mut result, unresolved): (Vec<_>, Vec<_>) =
            included_files.into_iter().partition(|path| {
                ignored_paths.contains(path.as_path())
                    || path
                        .ancestors()
                        .skip(1)
                        .any(|parent| ignored_directories.contains(parent))
            });
        // An ignored file inside an untracked directory is absent from the `--directory` listing.
        if !unresolved.is_empty() {
            let ignored_unresolved = git::check_ignore_in(root, &unresolved)?;
            result.extend(
                unresolved
                    .into_iter()
                    .filter(|path| ignored_unresolved.contains(path)),
            );
        }
        result.sort();
        return Ok(result);
    }

    // Without negation patterns in `.worktreeinclude`, any ignored directory covered by an
    // included directory matches every file inside it; `included_nested_repos` already identifies
    // nested git repositories/worktrees, so no 4th `git ls-files` scan is needed.
    if !saw_negation {
        let included_directories: HashSet<&Path> = included
            .iter()
            .filter(|entry| entry.directory)
            .map(|entry| entry.path.as_path())
            .collect();

        let mut dirs_with_nested_repos: HashSet<&Path> = HashSet::new();
        for repo in &included_nested_repos {
            for ancestor in repo.ancestors() {
                if let Some(&dir) = ignored_directories.get(ancestor) {
                    dirs_with_nested_repos.insert(dir);
                }
            }
        }

        let mut dirs_with_included_files: HashSet<&Path> = HashSet::new();
        let mut all_matched_dirs_covered = true;
        for file in &included_files {
            if let Some(&dir) = file
                .ancestors()
                .skip(1)
                .find_map(|ancestor| ignored_directories.get(ancestor))
            {
                if dirs_with_included_files.insert(dir)
                    && !dir
                        .ancestors()
                        .any(|ancestor| included_directories.contains(ancestor))
                {
                    all_matched_dirs_covered = false;
                    break;
                }
            }
        }

        if all_matched_dirs_covered {
            let collapsible_dirs: HashSet<&Path> = dirs_with_included_files
                .into_iter()
                .filter(|dir| !dirs_with_nested_repos.contains(dir))
                .collect();

            let mut result: Vec<PathBuf> = collapsible_dirs
                .iter()
                .map(|dir| dir.to_path_buf())
                .collect();
            let mut unresolved = Vec::new();
            for file in included_files {
                if ignored_paths.contains(file.as_path()) {
                    result.push(file);
                } else if let Some(&dir) = file
                    .ancestors()
                    .skip(1)
                    .find_map(|ancestor| ignored_directories.get(ancestor))
                {
                    if !collapsible_dirs.contains(dir) {
                        result.push(file);
                    }
                } else {
                    unresolved.push(file);
                }
            }
            if !unresolved.is_empty() {
                let ignored_unresolved = git::check_ignore_in(root, &unresolved)?;
                result.extend(
                    unresolved
                        .into_iter()
                        .filter(|path| ignored_unresolved.contains(path)),
                );
            }
            result.sort();
            return Ok(result);
        }
    }

    // Fallback: scope the file-level ignored listing to only the ignored directories that contain
    // at least one candidate file, avoiding full scans of unrelated ignored trees like `target/`.
    let mut candidate_dirs: HashSet<&Path> = HashSet::new();
    let mut outside_dirs_selected = Vec::new();
    let mut unresolved = Vec::new();
    for file in &included_files {
        if let Some(&dir) = file
            .ancestors()
            .skip(1)
            .find_map(|ancestor| ignored_directories.get(ancestor))
        {
            candidate_dirs.insert(dir);
        } else if ignored_paths.contains(file.as_path()) {
            outside_dirs_selected.push(file.clone());
        } else {
            unresolved.push(file.clone());
        }
    }

    if candidate_dirs.is_empty() {
        let mut result = outside_dirs_selected;
        if !unresolved.is_empty() {
            let ignored_unresolved = git::check_ignore_in(root, &unresolved)?;
            result.extend(
                unresolved
                    .into_iter()
                    .filter(|path| ignored_unresolved.contains(path)),
            );
        }
        result.sort();
        return Ok(result);
    }

    let candidate_dir_strs: Option<Vec<&str>> = if candidate_dirs.len() <= 64 {
        candidate_dirs.iter().map(|dir| dir.to_str()).collect()
    } else {
        None
    };
    let (scoped_entries, ignored_unresolved) = std::thread::scope(|scope| {
        let unresolved_handle = scope.spawn(|| {
            if unresolved.is_empty() {
                Ok(HashSet::new())
            } else {
                git::check_ignore_in(root, &unresolved)
            }
        });
        let scoped = match &candidate_dir_strs {
            Some(dirs) => git_ignored_paths_in_dirs(root, dirs),
            None => git_ignored_paths(root, &["--exclude-standard"], false),
        };
        (scoped, unresolved_handle.join().unwrap())
    });
    let (directories_without_files, ignored_files): (Vec<_>, Vec<_>) = scoped_entries?
        .into_iter()
        .partition(|entry| entry.directory);
    let ignored_unresolved = ignored_unresolved?;

    let mut dirs_with_nested_repos: HashSet<&Path> = HashSet::new();
    for entry in &directories_without_files {
        for ancestor in entry.path.ancestors() {
            if let Some(&dir) = candidate_dirs.get(ancestor) {
                dirs_with_nested_repos.insert(dir);
            }
        }
    }

    let included_files: HashSet<PathBuf> = included_files.into_iter().collect();
    let mut selected_in_dirs: Vec<PathBuf> = Vec::new();
    let mut dir_state: HashMap<&Path, (usize, bool)> = HashMap::new();
    for entry in ignored_files {
        let is_selected = included_files.contains(&entry.path);
        if let Some(&dir) = entry
            .path
            .ancestors()
            .skip(1)
            .find_map(|ancestor| candidate_dirs.get(ancestor))
        {
            let state = dir_state.entry(dir).or_insert((0, true));
            state.0 += 1;
            state.1 &= is_selected;
            if is_selected {
                selected_in_dirs.push(entry.path);
            }
        }
    }

    let collapsible_dirs: HashSet<&Path> = dir_state
        .into_iter()
        .filter(|(dir, (count, all_selected))| {
            *count > 0 && *all_selected && !dirs_with_nested_repos.contains(dir)
        })
        .map(|(dir, _)| dir)
        .collect();

    let mut result: Vec<PathBuf> = collapsible_dirs
        .iter()
        .map(|dir| dir.to_path_buf())
        .collect();
    result.extend(outside_dirs_selected);
    result.extend(
        unresolved
            .into_iter()
            .filter(|path| ignored_unresolved.contains(path)),
    );
    if collapsible_dirs.is_empty() {
        result.extend(selected_in_dirs);
    } else {
        result.extend(selected_in_dirs.into_iter().filter(|path| {
            !path
                .ancestors()
                .skip(1)
                .any(|ancestor| collapsible_dirs.contains(ancestor))
        }));
    }
    result.sort();
    Ok(result)
}

struct IgnoredPath {
    path: PathBuf,
    directory: bool,
}

fn git_ignored_paths(
    root: &Path,
    exclude_args: &[&str],
    directories: bool,
) -> Result<Vec<IgnoredPath>> {
    let mut args = vec!["ls-files", "--others", "--ignored", "-z"];
    args.extend_from_slice(exclude_args);
    if directories {
        args.extend_from_slice(&["--directory", "--no-empty-directory"]);
    }
    parse_ignored_paths(&git::git_output_raw_in(root, &args)?)
}

fn git_ignored_paths_in_dirs(root: &Path, dirs: &[&str]) -> Result<Vec<IgnoredPath>> {
    let mut args = Vec::with_capacity(dirs.len() + 7);
    args.extend_from_slice(&[
        "--literal-pathspecs",
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "-z",
        "--",
    ]);
    args.extend_from_slice(dirs);
    parse_ignored_paths(&git::git_output_raw_in(root, &args)?)
}

fn parse_ignored_paths(output: &[u8]) -> Result<Vec<IgnoredPath>> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    Ok(output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let directory = path.ends_with(b"/");
            let path = path.strip_suffix(b"/").unwrap_or(path);
            IgnoredPath {
                path: PathBuf::from(OsString::from_vec(path.to_vec())),
                directory,
            }
        })
        .collect())
}

enum CopyTask {
    File {
        index: usize,
        src: PathBuf,
        dst: PathBuf,
        parent_ensured: bool,
    },
    DirEntry {
        index: usize,
        entry: fs::DirEntry,
        dst_dir: PathBuf,
    },
}

/// Copy multiple entries from `root` to `wt_path` using a shared 4-worker pool.
/// When `names` is small and contains directories (e.g. `.env` + `node_modules/`),
/// top-level directories are expanded into child tasks so all 4 workers stay busy.
pub(super) fn copy_entries<'a>(
    root: &Path,
    wt_path: &Path,
    names: &[&'a str],
    excludes: &[PathBuf],
    remove_first: bool,
) -> Vec<(&'a str, Result<()>)> {
    if names.len() <= 64 {
        let mut results: Vec<Option<Result<()>>> = (0..names.len()).map(|_| None).collect();
        let mut tasks = Vec::new();
        let mut ensured_parents: HashSet<PathBuf> = HashSet::new();

        for (index, &name) in names.iter().enumerate() {
            let source = root.join(name);
            let target = wt_path.join(name);
            if remove_first {
                if let Err(err) = remove_existing(&target) {
                    results[index] = Some(Err(err));
                    continue;
                }
            }
            if excludes.iter().any(|ex| source.starts_with(ex)) {
                results[index] = Some(Ok(()));
                continue;
            }
            if source.is_dir() {
                match expand_directory_tasks(index, &source, &target, excludes, &mut tasks) {
                    Ok(()) => {}
                    Err(err) => results[index] = Some(Err(err)),
                }
            } else {
                let parent_ensured = target.parent().is_none_or(|parent| {
                    ensured_parents.contains(parent)
                        || fs::create_dir_all(parent)
                            .map(|()| {
                                ensured_parents.insert(parent.to_path_buf());
                            })
                            .is_ok()
                });
                tasks.push(CopyTask::File {
                    index,
                    src: source,
                    dst: target,
                    parent_ensured,
                });
            }
        }

        let task_results = crate::parallel::map_with_limit(&tasks, 4, |task| match task {
            CopyTask::File {
                index,
                src,
                dst,
                parent_ensured,
            } => {
                let res = if !*parent_ensured {
                    if let Some(parent) = dst.parent() {
                        fs::create_dir_all(parent)
                            .map_err(anyhow::Error::from)
                            .and_then(|()| copy_file(src, dst))
                    } else {
                        copy_file(src, dst)
                    }
                } else {
                    copy_file(src, dst)
                };
                (*index, res)
            }
            CopyTask::DirEntry {
                index,
                entry,
                dst_dir,
            } => (*index, copy_directory_entry(entry, dst_dir, excludes)),
        });

        for (index, res) in task_results {
            if let Err(err) = res {
                if !matches!(results[index], Some(Err(_))) {
                    results[index] = Some(Err(err));
                }
            }
        }

        return names
            .iter()
            .zip(results)
            .map(|(&name, res)| (name, res.unwrap_or(Ok(()))))
            .collect();
    }

    // Large entry list (e.g. thousands of globbed files): pre-create deduplicated parent
    // directories once when copying into a clean worktree, then fan out across 4 workers.
    let ensured_parents = if !remove_first {
        let mut parents = HashSet::new();
        for &name in names {
            let target = wt_path.join(name);
            if let Some(parent) = target.parent() {
                if !parents.contains(parent) && fs::create_dir_all(parent).is_ok() {
                    parents.insert(parent.to_path_buf());
                }
            }
        }
        Some(parents)
    } else {
        None
    };

    crate::parallel::map_with_limit(names, 4, |&name| {
        let source = root.join(name);
        let target = wt_path.join(name);
        let result = if remove_first {
            remove_existing(&target).and_then(|()| copy_recursive(&source, &target, excludes))
        } else if excludes.iter().any(|ex| source.starts_with(ex)) {
            Ok(())
        } else if source.is_dir() {
            copy_directory(&source, &target, excludes)
        } else {
            let parent_ready = ensured_parents
                .as_ref()
                .is_some_and(|set| target.parent().is_none_or(|p| set.contains(p)));
            if !parent_ready {
                if let Some(parent) = target.parent() {
                    if let Err(err) = fs::create_dir_all(parent) {
                        return (name, Err(err.into()));
                    }
                }
            }
            copy_file(&source, &target)
        };
        (name, result)
    })
}

fn expand_directory_tasks(
    index: usize,
    src: &Path,
    dst: &Path,
    excludes: &[PathBuf],
    tasks: &mut Vec<CopyTask>,
) -> Result<()> {
    let mut current_src = src.to_path_buf();
    let mut current_dst = dst.to_path_buf();
    loop {
        fs::create_dir_all(&current_dst)
            .with_context(|| format!("failed to create dir: {}", current_dst.display()))?;
        let entries: Vec<_> = fs::read_dir(&current_src)
            .with_context(|| format!("failed to read dir: {}", current_src.display()))?
            .collect::<io::Result<_>>()?;
        if let [entry] = entries.as_slice() {
            let next_src = entry.path();
            if !excludes.iter().any(|exclude| next_src.starts_with(exclude))
                && entry.file_type()?.is_dir()
            {
                current_dst = current_dst.join(entry.file_name());
                current_src = next_src;
                continue;
            }
        }
        for entry in entries {
            tasks.push(CopyTask::DirEntry {
                index,
                entry,
                dst_dir: current_dst.clone(),
            });
        }
        return Ok(());
    }
}

/// Recursively copy a file or directory from `src` to `dst`, skipping absolute paths in `excludes`.
pub(super) fn copy_recursive(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    debug_assert!(
        excludes.iter().all(|ex| ex.is_absolute()),
        "excludes must be absolute paths"
    );
    if excludes.iter().any(|ex| src.starts_with(ex)) {
        return Ok(());
    }
    if src.is_dir() {
        copy_directory(src, dst, excludes)?;
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(src, dst)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn copy_recursive_parallel(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    if excludes.iter().any(|ex| src.starts_with(ex)) || !src.is_dir() {
        return copy_recursive(src, dst, excludes);
    }
    let mut tasks = Vec::new();
    expand_directory_tasks(0, src, dst, excludes, &mut tasks)?;
    crate::parallel::map_with_limit(&tasks, 4, |task| match task {
        CopyTask::DirEntry { entry, dst_dir, .. } => copy_directory_entry(entry, dst_dir, excludes),
        CopyTask::File { src, dst, .. } => copy_file(src, dst),
    })
    .into_iter()
    .collect()
}

fn copy_directory(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create dir: {}", dst.display()))?;
    copy_directory_contents(src, dst, excludes)
}

fn copy_directory_child(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    create_dir_if_missing(dst)
        .with_context(|| format!("failed to create dir: {}", dst.display()))?;
    copy_directory_contents(src, dst, excludes)
}

fn copy_directory_contents(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read dir: {}", src.display()))?
    {
        copy_directory_entry(&entry?, dst, excludes)?;
    }
    Ok(())
}

fn create_dir_if_missing(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

fn copy_directory_entry(entry: &fs::DirEntry, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    let source = entry.path();
    if excludes.iter().any(|ex| source.starts_with(ex)) {
        return Ok(());
    }
    let target = dst.join(entry.file_name());
    let file_type = entry.file_type()?;
    if file_type.is_dir() || (file_type.is_symlink() && source.is_dir()) {
        copy_directory_child(&source, &target, excludes)
    } else {
        copy_file(&source, &target)
    }
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    if try_clonefile(src, dst) {
        return Ok(());
    }
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn try_clonefile(src: &Path, dst: &Path) -> bool {
    use std::ffi::{c_char, c_int, CString};
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        fn clonefile(src: *const c_char, dst: *const c_char, flags: u32) -> c_int;
    }

    let Ok(src_c) = CString::new(src.as_os_str().as_bytes()) else {
        return false;
    };
    let Ok(dst_c) = CString::new(dst.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `src_c` and `dst_c` are valid NUL-terminated C strings.
    unsafe { clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn worktreeinclude_mode_uses_last_value_when_duplicated() {
        let config = Config::from_entries(vec![
            ("waku.worktreeinclude".to_string(), "copy".to_string()),
            ("waku.worktreeinclude".to_string(), "link".to_string()),
        ]);
        assert_eq!(
            WorktreeIncludeMode::from_config(&config),
            WorktreeIncludeMode::Link
        );

        let config = Config::from_entries(vec![
            ("waku.worktreeinclude".to_string(), "link".to_string()),
            ("waku.worktreeinclude".to_string(), "ignore".to_string()),
        ]);
        assert_eq!(
            WorktreeIncludeMode::from_config(&config),
            WorktreeIncludeMode::Ignore
        );

        let config = Config::from_entries(vec![
            ("waku.worktreeinclude".to_string(), "ignore".to_string()),
            ("waku.worktreeinclude".to_string(), "copy".to_string()),
        ]);
        assert_eq!(
            WorktreeIncludeMode::from_config(&config),
            WorktreeIncludeMode::Copy
        );
    }

    #[test]
    fn worktreeinclude_mode_defaults_to_copy() {
        assert_eq!(
            WorktreeIncludeMode::from_config(&Config::empty()),
            WorktreeIncludeMode::Copy
        );
    }

    #[test]
    fn parallel_directory_copy_preserves_contents_links_and_exclusions() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source");
        let dst = tmp.path().join("destination");
        for index in 0..16 {
            let dir = src.join(index.to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("keep"), index.to_string()).unwrap();
            fs::write(dir.join("skip"), "excluded").unwrap();
        }
        std::os::unix::fs::symlink(src.join("0/keep"), src.join("file-link")).unwrap();
        std::os::unix::fs::symlink(src.join("0"), src.join("dir-link")).unwrap();
        let mut excludes: Vec<_> = (0..16)
            .map(|index| src.join(format!("{index}/skip")))
            .collect();
        excludes.push(src.join("dir-link/skip"));
        copy_recursive_parallel(&src, &dst, &excludes).unwrap();
        for index in 0..16 {
            assert_eq!(
                fs::read_to_string(dst.join(format!("{index}/keep"))).unwrap(),
                index.to_string()
            );
            assert!(!dst.join(format!("{index}/skip")).exists());
        }
        assert_eq!(fs::read_to_string(dst.join("file-link")).unwrap(), "0");
        assert_eq!(fs::read_to_string(dst.join("dir-link/keep")).unwrap(), "0");
        assert!(!dst.join("dir-link/skip").exists());
        assert!(!dst.join("file-link").is_symlink());
        assert!(!dst.join("dir-link").is_symlink());

        let excluded = tmp.path().join("excluded");
        copy_recursive_parallel(&src, &excluded, std::slice::from_ref(&src)).unwrap();
        assert!(!excluded.exists());
        std::os::unix::fs::symlink(src.join("missing"), src.join("broken-link")).unwrap();
        assert!(copy_recursive_parallel(&src, &dst, &[]).is_err());
    }

    #[test]
    fn parallel_directory_copy_preserves_directory_chains_and_exclusions() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source");
        let dst = tmp.path().join("destination");
        let files = src.join("cache/packages");
        fs::create_dir_all(files.join("empty")).unwrap();
        for index in 0..16 {
            fs::write(files.join(index.to_string()), index.to_string()).unwrap();
        }
        let excluded = files.join("3");
        copy_recursive_parallel(&src, &dst, &[excluded]).unwrap();
        assert!(dst.join("cache/packages/empty").is_dir());
        for index in 0..16 {
            let target = dst.join(format!("cache/packages/{index}"));
            if index == 3 {
                assert!(!target.exists());
            } else {
                assert_eq!(fs::read_to_string(target).unwrap(), index.to_string());
            }
        }

        let filtered = tmp.path().join("filtered");
        copy_recursive_parallel(&src, &filtered, &[files]).unwrap();
        assert!(filtered.join("cache").is_dir());
        assert!(!filtered.join("cache/packages").exists());
    }

    #[test]
    fn copy_recursive_with_excludes_skips_excluded_dir() {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let src = tmp.path().join("src_dir");
        let dst = tmp.path().join("dst_dir");

        // Build source tree: src_dir/{a.txt, cache/{big.dat}, keep/{ok.txt}}
        fs::create_dir_all(src.join("cache")).unwrap();
        fs::create_dir_all(src.join("keep")).unwrap();
        fs::write(src.join("a.txt"), "root file").unwrap();
        fs::write(src.join("cache/big.dat"), "should be excluded").unwrap();
        fs::write(src.join("keep/ok.txt"), "should be kept").unwrap();

        let excludes = vec![src.join("cache")];
        copy_recursive(&src, &dst, &excludes).unwrap();

        assert!(dst.join("a.txt").exists(), "a.txt should be copied");
        assert!(
            dst.join("keep/ok.txt").exists(),
            "keep/ok.txt should be copied"
        );
        assert!(!dst.join("cache").exists(), "cache dir should be excluded");
    }

    #[test]
    fn copy_recursive_with_excludes_skips_excluded_file() {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let src = tmp.path().join("src_dir");
        let dst = tmp.path().join("dst_dir");

        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("keep.txt"), "keep").unwrap();
        fs::write(src.join("secret.txt"), "exclude me").unwrap();

        let excludes = vec![src.join("secret.txt")];
        copy_recursive(&src, &dst, &excludes).unwrap();

        assert!(dst.join("keep.txt").exists(), "keep.txt should be copied");
        assert!(
            !dst.join("secret.txt").exists(),
            "secret.txt should be excluded"
        );
    }

    #[test]
    fn copy_recursive_with_excludes_no_false_prefix_match() {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let src = tmp.path().join("src_dir");
        let dst = tmp.path().join("dst_dir");

        // .cache should be excluded but .cache-v2 should NOT
        fs::create_dir_all(src.join(".cache")).unwrap();
        fs::create_dir_all(src.join(".cache-v2")).unwrap();
        fs::write(src.join(".cache/x"), "excluded").unwrap();
        fs::write(src.join(".cache-v2/y"), "kept").unwrap();

        let excludes = vec![src.join(".cache")];
        copy_recursive(&src, &dst, &excludes).unwrap();

        assert!(!dst.join(".cache").exists(), ".cache should be excluded");
        assert!(
            dst.join(".cache-v2/y").exists(),
            ".cache-v2 should NOT be excluded"
        );
    }

    #[test]
    fn copy_recursive_with_empty_excludes_copies_everything() {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let src = tmp.path().join("src_dir");
        let dst = tmp.path().join("dst_dir");

        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("sub/b.txt"), "b").unwrap();

        let excludes: Vec<PathBuf> = vec![];
        copy_recursive(&src, &dst, &excludes).unwrap();

        assert!(dst.join("a.txt").exists());
        assert!(dst.join("sub/b.txt").exists());
    }

    #[test]
    fn copy_recursive_follows_file_and_directory_symlinks() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source");
        let dst = tmp.path().join("destination");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("nested/file.txt"), "contents").unwrap();
        std::os::unix::fs::symlink("nested/file.txt", src.join("file-link")).unwrap();
        std::os::unix::fs::symlink("nested", src.join("directory-link")).unwrap();

        copy_recursive(&src, &dst, &[]).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("file-link")).unwrap(),
            "contents"
        );
        assert_eq!(
            fs::read_to_string(dst.join("directory-link/file.txt")).unwrap(),
            "contents"
        );
        assert!(!dst.join("file-link").is_symlink());
        assert!(!dst.join("directory-link").is_symlink());
    }

    #[test]
    fn copy_recursive_creates_parent_for_a_single_file_and_preserves_other_files() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("nested/destination.txt");
        fs::write(&src, "contents").unwrap();
        copy_recursive(&src, &dst, &[]).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "contents");

        let source_dir = tmp.path().join("source-dir");
        fs::create_dir(&source_dir).unwrap();
        fs::write(source_dir.join("another.txt"), "another").unwrap();
        copy_recursive(&source_dir, dst.parent().unwrap(), &[]).unwrap();
        assert_eq!(fs::read_to_string(&dst).unwrap(), "contents");
        assert_eq!(
            fs::read_to_string(dst.parent().unwrap().join("another.txt")).unwrap(),
            "another"
        );
    }

    #[test]
    fn copy_entries_copies_mixed_files_and_directories_in_parallel() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join(".env"), "SECRET=1").unwrap();
        for pkg in 0..8 {
            let dir = src.join(format!("node_modules/pkg-{pkg}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("index.js"), format!("pkg-{pkg}")).unwrap();
        }
        let excludes = vec![src.join("node_modules/pkg-3")];
        let results = copy_entries(&src, &dst, &[".env", "node_modules"], &excludes, false);
        assert_eq!(results.len(), 2);
        assert!(results[0].1.is_ok());
        assert!(results[1].1.is_ok());
        assert_eq!(fs::read_to_string(dst.join(".env")).unwrap(), "SECRET=1");
        assert!(!dst.join("node_modules/pkg-3").exists());
        for pkg in [0, 1, 2, 4, 5, 6, 7] {
            assert_eq!(
                fs::read_to_string(dst.join(format!("node_modules/pkg-{pkg}/index.js"))).unwrap(),
                format!("pkg-{pkg}")
            );
        }
    }
}
