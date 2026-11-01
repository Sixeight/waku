use std::collections::HashSet;
use std::fs;
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
        match waku_config.first("waku.worktreeinclude") {
            Some("link") => Self::Link,
            Some("ignore") => Self::Ignore,
            _ => Self::Copy,
        }
    }
}

/// Collect paths matching `.worktreeinclude` that are also gitignored.
/// Returns relative paths from `root`.
pub(super) fn collect_worktreeinclude_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.join(".worktreeinclude").exists() {
        return Ok(Vec::new());
    }

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
    let included_files: Vec<_> = included_entries?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    let has_positive_pattern_after_negation = fs::read_to_string(root.join(".worktreeinclude"))
        .ok()
        .is_none_or(|patterns| {
            let mut saw_negation = false;
            patterns.lines().any(|line| {
                if line.starts_with('!') {
                    saw_negation = true;
                    false
                } else {
                    saw_negation && !line.is_empty() && !line.starts_with('#')
                }
            })
        });
    if included.iter().all(|entry| !entry.directory) && !has_positive_pattern_after_negation {
        let ignored_paths: HashSet<_> = ignored.iter().map(|entry| entry.path.as_path()).collect();
        let ignored_directories: HashSet<_> = ignored
            .iter()
            .filter(|entry| entry.directory)
            .map(|entry| entry.path.as_path())
            .collect();
        let (mut result, unresolved): (Vec<_>, Vec<_>) =
            included_files.into_iter().partition(|path| {
                ignored_paths.contains(path.as_path())
                    || path
                        .ancestors()
                        .skip(1)
                        .any(|parent| ignored_directories.contains(parent))
            });
        // An ignored file inside an untracked directory is absent from the `--directory` listing.
        let ignored_unresolved = git::check_ignore_in(root, &unresolved)?;
        result.extend(
            unresolved
                .into_iter()
                .filter(|path| ignored_unresolved.contains(path)),
        );
        result.sort();
        return Ok(result);
    }

    let (directories_without_files, ignored_files): (Vec<_>, Vec<_>) =
        git_ignored_paths(root, &["--exclude-standard"], false)?
            .into_iter()
            .map(|entry| entry.path)
            .partition(|path| {
                let source = root.join(path);
                source.is_dir() && !source.is_symlink()
            });
    let included_files: HashSet<_> = included_files.into_iter().collect();
    let mut selected: HashSet<_> = ignored_files
        .iter()
        .filter(|path| included_files.contains(*path))
        .cloned()
        .collect();
    let mut result = Vec::new();
    for entry in ignored {
        if !entry.directory
            || directories_without_files
                .iter()
                .any(|path| path.starts_with(&entry.path))
        {
            continue;
        }
        let contents: Vec<_> = ignored_files
            .iter()
            .filter(|path| path.starts_with(&entry.path))
            .collect();
        if !contents.is_empty() && contents.iter().all(|path| selected.contains(*path)) {
            for path in contents {
                selected.remove(path);
            }
            result.push(entry.path);
        }
    }
    result.extend(selected);
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
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut args = vec!["ls-files", "--others", "--ignored", "-z"];
    args.extend_from_slice(exclude_args);
    if directories {
        args.extend_from_slice(&["--directory", "--no-empty-directory"]);
    }
    let output = git::git_output_raw_in(root, &args)?;
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

pub(super) fn copy_recursive_parallel(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    if excludes.iter().any(|ex| src.starts_with(ex)) || !src.is_dir() {
        return copy_recursive(src, dst, excludes);
    }
    fs::create_dir_all(dst).with_context(|| format!("failed to create dir: {}", dst.display()))?;
    let entries: Vec<_> = fs::read_dir(src)
        .with_context(|| format!("failed to read dir: {}", src.display()))?
        .collect::<std::io::Result<_>>()?;
    if let [entry] = entries.as_slice() {
        let source = entry.path();
        if !excludes.iter().any(|exclude| source.starts_with(exclude))
            && entry.file_type()?.is_dir()
        {
            return copy_recursive_parallel(&source, &dst.join(entry.file_name()), excludes);
        }
    }
    crate::parallel::map_with_limit(&entries, 4, |entry| {
        copy_directory_entry(entry, dst, excludes)
    })
    .into_iter()
    .collect()
}

fn copy_directory(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create dir: {}", dst.display()))?;
    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read dir: {}", src.display()))?
    {
        copy_directory_entry(&entry?, dst, excludes)?;
    }
    Ok(())
}

fn copy_directory_entry(entry: &fs::DirEntry, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    let source = entry.path();
    if excludes.iter().any(|ex| source.starts_with(ex)) {
        return Ok(());
    }
    let target = dst.join(entry.file_name());
    let file_type = entry.file_type()?;
    if file_type.is_dir() || (file_type.is_symlink() && source.is_dir()) {
        copy_directory(&source, &target, excludes)
    } else {
        copy_file(&source, &target)
    }
}

fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
