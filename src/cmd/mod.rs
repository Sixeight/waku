pub mod clean;
pub mod config;
pub mod create;
pub mod open;
pub mod path;
pub mod remove;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::git;

pub const SPINNER_TEMPLATE: &str = "  {prefix} {msg:.dim}{spinner:.dim}";

fn spinner_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .tick_strings(&["   ", ".  ", ".. ", "...", "   "])
        .template(SPINNER_TEMPLATE)
        .unwrap()
}

pub fn spinner(msg: String) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_prefix("ﾜ");
    pb.set_message(msg);
    pb.enable_steady_tick(Duration::from_millis(400));

    let pb2 = pb.clone();
    std::thread::spawn(move || {
        let mut waku = false;
        loop {
            std::thread::sleep(Duration::from_millis(120));
            if pb2.is_finished() {
                break;
            }
            waku = !waku;
            pb2.set_prefix(if waku { "ﾜ" } else { "ｸ" });
        }
    });

    pb
}

/// Pass unknown subcommands through to `git worktree`.
pub fn passthrough(args: &[String]) -> Result<()> {
    let code = git::git_passthrough(args)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

/// Remove an existing file, symlink, or directory at `path`.
/// No-op if the path does not exist.
pub fn remove_existing(path: &Path) -> Result<()> {
    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove existing: {}", path.display()))?;
    } else if path.exists() || path.is_symlink() {
        fs::remove_file(path)
            .with_context(|| format!("failed to remove existing: {}", path.display()))?;
    }
    Ok(())
}

/// Remove a directory if it is empty.
pub fn cleanup_empty_dirs(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut entries =
        fs::read_dir(dir).with_context(|| format!("failed to read dir: {}", dir.display()))?;
    if entries.next().transpose()?.is_none() {
        fs::remove_dir(dir)
            .with_context(|| format!("failed to remove empty dir: {}", dir.display()))?;
    }
    Ok(())
}

/// Extract the human-readable detail from a git error.
///
/// `git_output_in` produces messages like:
///   "git worktree remove /path failed: fatal: '/path' contains ..."
///
/// This function extracts the useful part after "failed: " and strips
/// the "fatal: " / "error: " prefix.
pub fn extract_git_detail(error: &anyhow::Error) -> String {
    let msg = error.to_string();
    let detail = msg
        .find("failed: ")
        .map(|i| &msg[i + "failed: ".len()..])
        .unwrap_or(&msg);
    detail
        .strip_prefix("fatal: ")
        .or_else(|| detail.strip_prefix("error: "))
        .unwrap_or(detail)
        .to_string()
}

/// Print a warning message with colored output.
pub fn print_warning(context: &str, error: &anyhow::Error) {
    use console::style;
    let detail = extract_git_detail(error);
    eprintln!("{}: {}", style("warning").yellow().bold(), context);
    eprintln!("      {} {}", style("→").dim(), detail);
}

/// Resolve the configured command line for a tool, with defaults.
/// The last matching entry wins, mirroring `git config --get` semantics.
pub fn resolve_tool(config: &[(String, String)], tool: &str) -> String {
    let key = format!("waku.command.{tool}");
    config
        .iter()
        .rev()
        .find(|(k, _)| k == &key)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| match tool {
            "agent" => "claude".to_string(),
            _ => "nvim".to_string(),
        })
}

/// Resolve the command and configured arguments for a tool.
pub fn resolve_tool_command(
    config: &[(String, String)],
    tool: &str,
) -> Result<(String, Vec<String>)> {
    let command_line = resolve_tool(config, tool);
    parse_command_line(&command_line)
}

/// Resolve a command line from pre-loaded config, preferring a one-time override.
pub fn resolve_tool_command_with_override(
    config: &[(String, String)],
    tool: &str,
    command_override: Option<&str>,
) -> Result<(String, Vec<String>)> {
    match command_override {
        Some(command_line) => parse_command_line(command_line),
        None => resolve_tool_command(config, tool),
    }
}

fn parse_command_line(command_line: &str) -> Result<(String, Vec<String>)> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = command_line.chars().peekable();
    let mut quote = None;

    while let Some(ch) = chars.next() {
        match quote {
            Some(active_quote) => match ch {
                '\\' if active_quote == '"' => {
                    let Some(next) = chars.next() else {
                        bail!("unterminated escape in command: {command_line}");
                    };
                    current.push(next);
                }
                q if q == active_quote => quote = None,
                _ => current.push(ch),
            },
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    let Some(next) = chars.next() else {
                        bail!("unterminated escape in command: {command_line}");
                    };
                    current.push(next);
                }
                ch if ch.is_whitespace() => {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            },
        }
    }

    if let Some(active_quote) = quote {
        bail!("unterminated quote {active_quote} in command: {command_line}");
    }

    if !current.is_empty() {
        args.push(current);
    }

    let Some((program, args)) = args.split_first() else {
        bail!("empty command is not allowed");
    };

    Ok((program.clone(), args.to_vec()))
}

/// The mode for handling `.worktreeinclude` entries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorktreeIncludeMode {
    Copy,
    Link,
    Ignore,
}

impl WorktreeIncludeMode {
    pub fn from_config(waku_config: &[(String, String)]) -> Self {
        waku_config
            .iter()
            .find(|(k, _)| k == "waku.worktreeinclude")
            .map(|(_, v)| match v.as_str() {
                "link" => Self::Link,
                "ignore" => Self::Ignore,
                _ => Self::Copy,
            })
            .unwrap_or(Self::Copy)
    }
}

/// Collect paths matching `.worktreeinclude` that are also gitignored.
/// Returns relative paths from `root`.
pub fn collect_worktreeinclude_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.join(".worktreeinclude").exists() {
        return Ok(Vec::new());
    }

    // Combining exclude sources would select their union rather than their intersection.
    let ignored_entries = git_ignored_paths(root, &["--exclude-standard"])?;
    let (directories_without_files, ignored): (Vec<_>, Vec<_>) =
        ignored_entries.into_iter().partition(|path| {
            let source = root.join(path);
            source.is_dir() && !source.is_symlink()
        });
    let included: HashSet<PathBuf> = git_ignored_paths(root, &["--exclude-from=.worktreeinclude"])?
        .into_iter()
        .collect();
    let mut selected: HashSet<PathBuf> = ignored
        .iter()
        .filter(|path| included.contains(*path))
        .cloned()
        .collect();

    let directories = git_ignored_paths(
        root,
        &["--exclude-standard", "--directory", "--no-empty-directory"],
    )?;
    let mut result = Vec::new();
    for directory in directories {
        let source = root.join(&directory);
        if !source.is_dir()
            || source.is_symlink()
            || directories_without_files
                .iter()
                .any(|path| path.starts_with(&directory))
        {
            continue;
        }
        let contents: Vec<_> = ignored
            .iter()
            .filter(|path| path.starts_with(&directory))
            .collect();
        // A directory link or recursive copy must not expose excluded contents.
        if !contents.is_empty() && contents.iter().all(|path| selected.contains(*path)) {
            for path in contents {
                selected.remove(path);
            }
            result.push(directory);
        }
    }
    result.extend(selected);
    result.sort();
    Ok(result)
}

fn git_ignored_paths(root: &Path, exclude_args: &[&str]) -> Result<Vec<PathBuf>> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut args = vec!["ls-files", "--others", "--ignored", "-z"];
    args.extend_from_slice(exclude_args);
    let output = git::git_output_raw_in(root, &args)?;
    Ok(output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = path.strip_suffix(b"/").unwrap_or(path);
            PathBuf::from(OsString::from_vec(path.to_vec()))
        })
        .collect())
}

/// Extract values for a given config key.
pub fn config_values<'a>(config: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    config
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

pub fn config_bool(config: &[(String, String)], key: &str) -> bool {
    config
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| matches!(v.as_str(), "true" | "yes" | "on" | "1"))
        .unwrap_or(false)
}

/// Recursively copy a file or directory from `src` to `dst`, skipping absolute paths in `excludes`.
pub fn copy_recursive(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
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

pub fn copy_recursive_parallel(src: &Path, dst: &Path, excludes: &[PathBuf]) -> Result<()> {
    if excludes.iter().any(|ex| src.starts_with(ex)) || !src.is_dir() {
        return copy_recursive(src, dst, excludes);
    }
    fs::create_dir_all(dst).with_context(|| format!("failed to create dir: {}", dst.display()))?;
    let entries: Vec<_> = fs::read_dir(src)
        .with_context(|| format!("failed to read dir: {}", src.display()))?
        .collect::<std::io::Result<_>>()?;
    // Splitting only the top directory keeps nested trees within the worker limit.
    crate::parallel::map(&entries, |entry| copy_directory_entry(entry, dst, excludes))
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
    fn extract_git_detail_strips_fatal_prefix() {
        let err = anyhow::anyhow!(
            "git worktree remove /tmp/repo failed: fatal: '/tmp/repo' contains modified or untracked files, use --force to delete"
        );
        let detail = extract_git_detail(&err);
        assert_eq!(
            detail,
            "'/tmp/repo' contains modified or untracked files, use --force to delete"
        );
    }

    #[test]
    fn extract_git_detail_strips_error_prefix() {
        let err = anyhow::anyhow!(
            "git branch -d feature failed: error: The branch 'feature' is not fully merged."
        );
        let detail = extract_git_detail(&err);
        assert_eq!(detail, "The branch 'feature' is not fully merged.");
    }

    #[test]
    fn extract_git_detail_no_failed_prefix() {
        let err = anyhow::anyhow!("something unexpected happened");
        let detail = extract_git_detail(&err);
        assert_eq!(detail, "something unexpected happened");
    }

    #[test]
    fn extract_git_detail_failed_without_fatal() {
        let err = anyhow::anyhow!("git fetch failed: could not resolve host");
        let detail = extract_git_detail(&err);
        assert_eq!(detail, "could not resolve host");
    }

    #[test]
    fn resolve_tool_defaults() {
        let config: Vec<(String, String)> = vec![];
        assert_eq!(resolve_tool(&config, "agent"), "claude");
        assert_eq!(resolve_tool(&config, "editor"), "nvim");
    }

    #[test]
    fn resolve_tool_from_config() {
        let config = vec![
            ("waku.command.agent".to_string(), "aider".to_string()),
            ("waku.command.editor".to_string(), "vim".to_string()),
        ];
        assert_eq!(resolve_tool(&config, "agent"), "aider");
        assert_eq!(resolve_tool(&config, "editor"), "vim");
    }

    #[test]
    fn resolve_tool_prefers_last_value_like_git_config_get() {
        // git config --get-regexp lists system → global → local in order,
        // and `git config --get` returns the last (most specific) value.
        let config = vec![
            ("waku.command.agent".to_string(), "global-agent".to_string()),
            ("waku.command.agent".to_string(), "local-agent".to_string()),
        ];
        assert_eq!(resolve_tool(&config, "agent"), "local-agent");
    }

    #[test]
    fn resolve_tool_command_splits_configured_arguments() {
        let config = vec![(
            "waku.command.agent".to_string(),
            "claude --resume --model sonnet".to_string(),
        )];
        let (program, args) = resolve_tool_command(&config, "agent").unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--resume", "--model", "sonnet"]);
    }

    #[test]
    fn resolve_tool_command_preserves_quoted_arguments() {
        let config = vec![(
            "waku.command.agent".to_string(),
            "claude --append \"hello world\"".to_string(),
        )];
        let (program, args) = resolve_tool_command(&config, "agent").unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--append", "hello world"]);
    }

    #[test]
    fn config_values_filters_by_key() {
        let config = vec![
            ("waku.link.include".to_string(), "node_modules".to_string()),
            ("waku.copy.include".to_string(), ".env".to_string()),
            ("waku.link.include".to_string(), ".direnv".to_string()),
        ];
        assert_eq!(
            config_values(&config, "waku.link.include"),
            vec!["node_modules", ".direnv"]
        );
        assert_eq!(config_values(&config, "waku.copy.include"), vec![".env"]);
    }

    #[test]
    fn config_values_returns_empty_for_missing_key() {
        let config = vec![("waku.link.include".to_string(), "node_modules".to_string())];
        let result: Vec<&str> = config_values(&config, "waku.copy.include");
        assert!(result.is_empty());
    }

    #[test]
    fn config_values_empty_config() {
        let config: Vec<(String, String)> = vec![];
        let result: Vec<&str> = config_values(&config, "waku.link.include");
        assert!(result.is_empty());
    }

    #[test]
    fn config_bool_reads_last_matching_value() {
        let config = vec![
            ("waku.create.fetch".to_string(), "false".to_string()),
            ("waku.create.fetch".to_string(), "true".to_string()),
        ];
        assert!(config_bool(&config, "waku.create.fetch"));
    }

    #[test]
    fn config_bool_returns_false_for_missing_key() {
        let config: Vec<(String, String)> = vec![];
        assert!(!config_bool(&config, "waku.create.fetch"));
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
    fn spinner_template_has_dots_after_message() {
        let msg_pos = SPINNER_TEMPLATE
            .find("{msg")
            .expect("{msg} should exist in template");
        let spinner_pos = SPINNER_TEMPLATE
            .find("{spinner")
            .expect("{spinner} should exist in template");
        assert!(
            spinner_pos > msg_pos,
            "{{spinner}} (dots) must appear after {{msg}} in template: {SPINNER_TEMPLATE}"
        );
    }
}
