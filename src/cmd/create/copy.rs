use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

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

    // When every pattern in `.worktreeinclude` is a root-anchored literal path
    // (e.g. `/.env`, `apps/web/.env`, `/node_modules/`), scope `git ls-files` to
    // those pathspecs so unrelated ignored trees like `target/` are never scanned.
    let anchored_paths = anchored_literal_patterns(&wti_contents);
    let pathspec_refs: Option<Vec<&str>> = anchored_paths
        .as_ref()
        .map(|paths| paths.iter().map(String::as_str).collect());
    if let Some(ref paths) = pathspec_refs {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
    }

    // Combining exclude sources would select their union rather than their intersection.
    // `--directory` omits matches inside an untracked directory that is only partly matched,
    // so the file-level listing supplies the candidates.
    let (ignored, included, included_entries) = std::thread::scope(|scope| {
        let pathspecs = pathspec_refs.as_deref();
        let ignored = scope.spawn(move || {
            git_ignored_paths_scoped(root, &["--exclude-standard"], true, pathspecs)
        });
        let included = scope.spawn(move || {
            git_ignored_paths_scoped(root, &["--exclude-from=.worktreeinclude"], true, pathspecs)
        });
        let included_entries = scope.spawn(move || {
            git_ignored_paths_scoped(root, &["--exclude-from=.worktreeinclude"], false, pathspecs)
        });
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

fn anchored_literal_patterns(contents: &str) -> Option<Vec<String>> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for raw_line in contents.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('!') || line.contains(['*', '?', '[', ']', '\\']) {
            return None;
        }
        let line = line.trim_end_matches(' ');
        if line.is_empty() {
            continue;
        }
        let had_leading_slash = line.starts_with('/');
        let trimmed = line.trim_start_matches('/').trim_end_matches('/');
        if trimmed.is_empty() {
            return None;
        }
        // In gitignore syntax, a pattern without a leading or internal slash matches
        // in any subdirectory, so it cannot be scoped to a single root pathspec.
        if !had_leading_slash && !trimmed.contains('/') {
            return None;
        }
        if trimmed
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return None;
        }
        if seen.insert(trimmed.to_string()) {
            paths.push(trimmed.to_string());
        }
        if paths.len() > 64 {
            return None;
        }
    }
    Some(paths)
}

fn git_ignored_paths(
    root: &Path,
    exclude_args: &[&str],
    directories: bool,
) -> Result<Vec<IgnoredPath>> {
    git_ignored_paths_scoped(root, exclude_args, directories, None)
}

fn git_ignored_paths_scoped(
    root: &Path,
    exclude_args: &[&str],
    directories: bool,
    pathspecs: Option<&[&str]>,
) -> Result<Vec<IgnoredPath>> {
    let mut args =
        Vec::with_capacity(exclude_args.len() + 8 + pathspecs.map_or(0, |specs| specs.len() + 1));
    if pathspecs.is_some() {
        args.push("--literal-pathspecs");
    }
    args.extend_from_slice(&["ls-files", "--others", "--ignored", "-z"]);
    args.extend_from_slice(exclude_args);
    if directories {
        args.extend_from_slice(&["--directory", "--no-empty-directory"]);
    }
    if let Some(specs) = pathspecs {
        args.push("--");
        args.extend_from_slice(specs);
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

pub(super) struct CopyTarget<'a> {
    pub(super) name: &'a str,
    pub(super) src: PathBuf,
    pub(super) dst: PathBuf,
}

enum CopyWork {
    File {
        index: usize,
        src: PathBuf,
        dst: PathBuf,
        parent_ensured: bool,
    },
    Dir {
        index: usize,
        src: PathBuf,
        dst: PathBuf,
        parent_ensured: bool,
    },
}

struct WorkQueue {
    items: Vec<CopyWork>,
    in_flight: usize,
}

/// Copy multiple entries from `root` to `wt_path` using a shared 4-worker pool.
pub(super) fn copy_entries<'a>(
    root: &Path,
    wt_path: &Path,
    names: &[&'a str],
    excludes: &[PathBuf],
    remove_first: bool,
) -> Vec<(&'a str, Result<()>)> {
    let targets: Vec<CopyTarget<'a>> = names
        .iter()
        .map(|&name| CopyTarget {
            name,
            src: root.join(name),
            dst: wt_path.join(name),
        })
        .collect();
    copy_entries_to(&targets, excludes, remove_first)
}

/// Copy explicit `(src -> dst)` targets using a shared 4-worker work-stealing pool.
/// Subdirectories discovered at any depth are pushed onto the shared queue so skewed
/// trees (e.g. `node_modules/.pnpm/`) keep all 4 workers busy.
pub(super) fn copy_entries_to<'a>(
    targets: &[CopyTarget<'a>],
    excludes: &[PathBuf],
    remove_first: bool,
) -> Vec<(&'a str, Result<()>)> {
    if targets.is_empty() {
        return Vec::new();
    }

    if targets.len() <= 64 {
        let mut results: Vec<Option<Result<()>>> = (0..targets.len()).map(|_| None).collect();
        let mut initial_work = Vec::with_capacity(targets.len());
        let mut ensured_parents: HashSet<PathBuf> = HashSet::new();
        let mut has_dir = false;

        for (index, target) in targets.iter().enumerate() {
            if remove_first {
                if let Err(err) = remove_existing(&target.dst) {
                    results[index] = Some(Err(err));
                    continue;
                }
            }
            if is_excluded(&target.src, excludes) {
                results[index] = Some(Ok(()));
                continue;
            }
            if target.src.is_dir() {
                has_dir = true;
                initial_work.push(CopyWork::Dir {
                    index,
                    src: target.src.clone(),
                    dst: target.dst.clone(),
                    parent_ensured: false,
                });
            } else {
                let parent_ensured = target.dst.parent().is_none_or(|parent| {
                    ensured_parents.contains(parent)
                        || fs::create_dir_all(parent)
                            .map(|()| {
                                ensured_parents.insert(parent.to_path_buf());
                            })
                            .is_ok()
                });
                initial_work.push(CopyWork::File {
                    index,
                    src: target.src.clone(),
                    dst: target.dst.clone(),
                    parent_ensured,
                });
            }
        }

        if !initial_work.is_empty() {
            if !has_dir && initial_work.len() == 1 {
                if let CopyWork::File {
                    index,
                    src,
                    dst,
                    parent_ensured,
                } = initial_work.pop().unwrap()
                {
                    let res = copy_file_with_parent(&src, &dst, parent_ensured);
                    if let Err(err) = res {
                        results[index] = Some(Err(err));
                    }
                }
            } else {
                let worker_count = if has_dir {
                    4
                } else {
                    initial_work.len().min(4)
                }
                .min(
                    std::thread::available_parallelism()
                        .map(usize::from)
                        .unwrap_or(1),
                )
                .max(1);
                let queue = (
                    Mutex::new(WorkQueue {
                        items: initial_work,
                        in_flight: 0,
                    }),
                    Condvar::new(),
                );
                let errors: Mutex<Vec<(usize, anyhow::Error)>> = Mutex::new(Vec::new());

                std::thread::scope(|scope| {
                    for _ in 0..worker_count {
                        scope.spawn(|| {
                            copy_worker(&queue, &errors, excludes);
                        });
                    }
                });

                for (index, err) in errors.into_inner().unwrap() {
                    if !matches!(results[index], Some(Err(_))) {
                        results[index] = Some(Err(err));
                    }
                }
            }
        }

        return targets
            .iter()
            .zip(results)
            .map(|(target, res)| (target.name, res.unwrap_or(Ok(()))))
            .collect();
    }

    // Large entry list (e.g. thousands of globbed files): pre-create deduplicated parent
    // directories once when copying into a clean destination, then fan out across 4 workers.
    let ensured_parents = if !remove_first {
        let mut parents = HashSet::new();
        for target in targets {
            if let Some(parent) = target.dst.parent() {
                if !parents.contains(parent) && fs::create_dir_all(parent).is_ok() {
                    parents.insert(parent.to_path_buf());
                }
            }
        }
        Some(parents)
    } else {
        None
    };

    crate::parallel::map_with_limit(targets, 4, |target| {
        let result = if remove_first {
            remove_existing(&target.dst)
                .and_then(|()| copy_recursive(&target.src, &target.dst, excludes))
        } else if is_excluded(&target.src, excludes) {
            Ok(())
        } else if target.src.is_dir() {
            copy_directory(&target.src, &target.dst, excludes)
        } else {
            let parent_ready = ensured_parents
                .as_ref()
                .is_some_and(|set| target.dst.parent().is_none_or(|p| set.contains(p)));
            copy_file_with_parent(&target.src, &target.dst, parent_ready)
        };
        (target.name, result)
    })
}

fn is_excluded(path: &Path, excludes: &[PathBuf]) -> bool {
    !excludes.is_empty() && excludes.iter().any(|ex| path.starts_with(ex))
}

fn copy_file_with_parent(src: &Path, dst: &Path, parent_ensured: bool) -> Result<()> {
    if !parent_ensured {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
    }
    copy_file(src, dst)
}

fn copy_worker(
    queue: &(Mutex<WorkQueue>, Condvar),
    errors: &Mutex<Vec<(usize, anyhow::Error)>>,
    excludes: &[PathBuf],
) {
    let (lock, cvar) = queue;
    loop {
        let work = {
            let mut state = lock.lock().unwrap();
            loop {
                if let Some(item) = state.items.pop() {
                    state.in_flight += 1;
                    break item;
                }
                if state.in_flight == 0 {
                    return;
                }
                state = cvar.wait(state).unwrap();
            }
        };

        match work {
            CopyWork::File {
                index,
                src,
                dst,
                parent_ensured,
            } => {
                if let Err(err) = copy_file_with_parent(&src, &dst, parent_ensured) {
                    errors.lock().unwrap().push((index, err));
                }
            }
            CopyWork::Dir {
                index,
                src,
                dst,
                parent_ensured,
            } => {
                if let Err(err) =
                    process_directory_work(index, src, dst, parent_ensured, queue, excludes)
                {
                    errors.lock().unwrap().push((index, err));
                }
            }
        }

        let mut state = lock.lock().unwrap();
        state.in_flight -= 1;
        if state.items.is_empty() && state.in_flight == 0 {
            cvar.notify_all();
        }
    }
}

fn process_directory_work(
    index: usize,
    mut src: PathBuf,
    mut dst: PathBuf,
    mut parent_ensured: bool,
    queue: &(Mutex<WorkQueue>, Condvar),
    excludes: &[PathBuf],
) -> Result<()> {
    let (lock, cvar) = queue;
    loop {
        if parent_ensured {
            create_dir_if_missing(&dst)
                .with_context(|| format!("failed to create dir: {}", dst.display()))?;
        } else {
            fs::create_dir_all(&dst)
                .with_context(|| format!("failed to create dir: {}", dst.display()))?;
        }

        let mut subdirs = Vec::new();
        let mut files = Vec::new();
        for entry in
            fs::read_dir(&src).with_context(|| format!("failed to read dir: {}", src.display()))?
        {
            let entry = entry?;
            let source = entry.path();
            if is_excluded(&source, excludes) {
                continue;
            }
            let target = dst.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() || (file_type.is_symlink() && source.is_dir()) {
                subdirs.push((source, target));
            } else {
                files.push((source, target));
            }
        }

        // If this directory contains only subdirectories, keep one inline to avoid
        // queue churn on single-child chains while pushing the rest for other workers.
        let inline_next = if files.is_empty() && !subdirs.is_empty() {
            subdirs.pop()
        } else {
            None
        };

        if !subdirs.is_empty() {
            let mut state = lock.lock().unwrap();
            for (sub_src, sub_dst) in subdirs {
                state.items.push(CopyWork::Dir {
                    index,
                    src: sub_src,
                    dst: sub_dst,
                    parent_ensured: true,
                });
            }
            cvar.notify_all();
        }

        for (file_src, file_dst) in files {
            copy_file(&file_src, &file_dst)?;
        }

        match inline_next {
            Some((next_src, next_dst)) => {
                src = next_src;
                dst = next_dst;
                parent_ensured = true;
            }
            None => return Ok(()),
        }
    }
}

/// Recursively copy a file or directory from `src` to `dst`, skipping absolute paths in `excludes`.
pub(super) fn copy_recursive(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    debug_assert!(
        excludes.iter().all(|ex| ex.is_absolute()),
        "excludes must be absolute paths"
    );
    if is_excluded(src, excludes) {
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
    let target = CopyTarget {
        name: "",
        src: src.to_path_buf(),
        dst: dst.to_path_buf(),
    };
    copy_entries_to(&[target], excludes, false)
        .into_iter()
        .next()
        .unwrap()
        .1
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

    #[test]
    fn copy_entries_copies_deep_skewed_trees_with_work_stealing() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::create_dir_all(src.join("node_modules/.bin")).unwrap();
        fs::write(src.join("node_modules/.bin/tool"), "#!/bin/sh\n").unwrap();
        for pkg in 0..16 {
            let dir = src.join(format!("node_modules/.pnpm/pkg-{pkg}/lib"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("index.js"), format!("pkg-{pkg}")).unwrap();
        }
        let excludes = vec![src.join("node_modules/.pnpm/pkg-7")];
        let results = copy_entries(&src, &dst, &["node_modules"], &excludes, false);
        assert!(results[0].1.is_ok());
        assert_eq!(
            fs::read_to_string(dst.join("node_modules/.bin/tool")).unwrap(),
            "#!/bin/sh\n"
        );
        assert!(!dst.join("node_modules/.pnpm/pkg-7").exists());
        for pkg in (0..16).filter(|&p| p != 7) {
            assert_eq!(
                fs::read_to_string(dst.join(format!("node_modules/.pnpm/pkg-{pkg}/lib/index.js")))
                    .unwrap(),
                format!("pkg-{pkg}")
            );
        }
    }

    #[test]
    fn anchored_literal_patterns_extracts_only_root_anchored_literals() {
        assert_eq!(
            anchored_literal_patterns("# comment\n/.env\napps/web/.env\n/node_modules/\n"),
            Some(vec![
                ".env".to_string(),
                "apps/web/.env".to_string(),
                "node_modules".to_string(),
            ])
        );
        assert_eq!(anchored_literal_patterns(".env\n"), None);
        assert_eq!(anchored_literal_patterns("node_modules/\n"), None);
        assert_eq!(anchored_literal_patterns("/.env\n!/.env\n"), None);
        assert_eq!(anchored_literal_patterns("apps/*/.env\n"), None);
    }
}
