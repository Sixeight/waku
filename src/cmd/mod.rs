pub mod clean;
pub mod create;
pub mod open;
pub mod path;
pub mod remove;

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};

use crate::{git, worktree};

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

/// Resolve a branch name to a worktree directory, falling back to the current directory.
pub fn resolve_dir(branch: Option<&str>) -> Result<PathBuf> {
    match branch {
        Some(b) => worktree::resolve_worktree(b),
        None => Ok(env::current_dir()?),
    }
}

/// Remove a directory if it is empty.
pub fn cleanup_empty_dirs(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("failed to read dir: {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    if entries.is_empty() {
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
    eprintln!(
        "{}: {}",
        style("warning").yellow().bold(),
        context
    );
    eprintln!(
        "      {} {}",
        style("→").dim(),
        detail
    );
}

/// Resolve the configured command line for a tool, with defaults.
pub fn resolve_tool(config: &[(String, String)], tool: &str) -> String {
    let key = format!("waku.command.{tool}");
    config
        .iter()
        .find(|(k, _)| k == &key)
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| match tool {
            "ai" => "claude".to_string(),
            _ => "nvim".to_string(),
        })
}

/// Resolve the configured command line for a tool from a repository root.
pub fn resolve_tool_in(root: &Path, tool: &str) -> Result<String> {
    let key = format!("waku.command.{tool}");
    Ok(git::config_get_in(root, &key)?.unwrap_or_else(|| match tool {
        "ai" => "claude".to_string(),
        _ => "nvim".to_string(),
    }))
}

/// Resolve the command and configured arguments for a tool.
pub fn resolve_tool_command(config: &[(String, String)], tool: &str) -> Result<(String, Vec<String>)> {
    let command_line = resolve_tool(config, tool);
    parse_command_line(&command_line)
}

/// Resolve the command and configured arguments for a tool from a repository root.
pub fn resolve_tool_command_in(root: &Path, tool: &str) -> Result<(String, Vec<String>)> {
    let command_line = resolve_tool_in(root, tool)?;
    parse_command_line(&command_line)
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

fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Collect files matching `.worktreeinclude` patterns that are also gitignored.
/// Returns relative paths from `root`.
pub fn collect_worktreeinclude_files(root: &Path) -> Result<Vec<PathBuf>> {
    let wti_path = root.join(".worktreeinclude");
    if !wti_path.exists() {
        return Ok(vec![]);
    }

    let wti_content = fs::read_to_string(&wti_path)
        .with_context(|| format!("failed to read {}", wti_path.display()))?;

    let mut glob_patterns = Vec::new();
    let mut literal_entries = Vec::new();

    for raw in wti_content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.trim_end_matches('/');

        if is_glob_pattern(line) {
            glob_patterns.push(line.to_string());
        } else if root.join(line).exists() {
            literal_entries.push(line.to_string());
        }
    }

    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    // Glob patterns: use fd/find for fast filename-based search,
    // then verify matches client-side with glob::Pattern.
    if !glob_patterns.is_empty() {
        let compiled: Vec<glob::Pattern> = glob_patterns
            .iter()
            .filter_map(|p| glob::Pattern::new(p).ok())
            .collect();

        let name_patterns: Vec<&str> = glob_patterns
            .iter()
            .map(|p| extract_name_pattern(p))
            .collect();

        let found = find_files_by_name(root, &name_patterns)?;
        for file in found {
            if compiled.iter().any(|pat| pat.matches(&file)) && seen.insert(file.clone()) {
                candidates.push(file);
            }
        }
    }

    // Literal entries: already verified to exist above.
    for entry in literal_entries {
        if seen.insert(entry.clone()) {
            candidates.push(entry);
        }
    }

    // Filter all candidates through git check-ignore to keep only gitignored files.
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    let ignored = git_check_ignore(root, &candidates)?;
    Ok(candidates
        .into_iter()
        .filter(|c| ignored.contains(c.as_str()))
        .map(PathBuf::from)
        .collect())
}

/// Extract the filename part from a glob pattern for use with fd/find.
/// e.g., `**/.env` → `.env`, `config/**/*.json` → `*.json`
fn extract_name_pattern(pattern: &str) -> &str {
    Path::new(pattern)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(pattern)
}

/// Convert a glob name pattern to a regex for fd.
/// e.g., `.env` → `^\.env$`, `.env*.local` → `^\.env.*\.local$`
fn glob_name_to_regex(name: &str) -> String {
    let mut regex = String::from("^");
    for c in name.chars() {
        match c {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' | '+' | '(' | ')' | '{' | '}' | '|' | '^' | '$' | '\\' => {
                regex.push('\\');
                regex.push(c);
            }
            _ => regex.push(c),
        }
    }
    regex.push('$');
    regex
}

/// Find files by name patterns using fd (fast, parallel) or find (fallback).
fn find_files_by_name(root: &Path, patterns: &[&str]) -> Result<Vec<String>> {
    // Deduplicate name patterns
    let unique: Vec<&str> = {
        let mut seen = HashSet::new();
        patterns.iter().filter(|p| seen.insert(**p)).copied().collect()
    };

    // Try fd first: single traversal with combined regex
    if let Ok(files) = find_with_fd(root, &unique) {
        return Ok(files);
    }

    // Fall back to find
    find_with_find(root, &unique)
}

fn stdout_lines(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn find_with_fd(root: &Path, patterns: &[&str]) -> Result<Vec<String>> {
    let regex = patterns
        .iter()
        .map(|p| glob_name_to_regex(p))
        .collect::<Vec<_>>()
        .join("|");

    let output = std::process::Command::new("fd")
        .args(["--no-ignore", "--hidden", "--type", "f", "--regex", &regex])
        .current_dir(root)
        .output()
        .context("fd not found")?;

    if !output.status.success() {
        anyhow::bail!("fd failed");
    }

    Ok(stdout_lines(&output.stdout))
}

fn find_with_find(root: &Path, patterns: &[&str]) -> Result<Vec<String>> {
    let mut cmd = std::process::Command::new("find");
    cmd.arg(".").args(["-not", "-path", "*/.git/*", "-type", "f"]);

    // Build: \( -name 'pat1' -o -name 'pat2' \)
    cmd.arg("(");
    for (i, pat) in patterns.iter().enumerate() {
        if i > 0 {
            cmd.arg("-o");
        }
        cmd.args(["-name", pat]);
    }
    cmd.arg(")");

    let output = cmd
        .current_dir(root)
        .output()
        .context("failed to execute find")?;

    Ok(stdout_lines(&output.stdout)
        .into_iter()
        .map(|l| l.strip_prefix("./").unwrap_or(&l).to_string())
        .collect())
}

/// Run `git check-ignore` on a list of paths and return the ignored ones.
fn git_check_ignore(root: &Path, paths: &[String]) -> Result<HashSet<String>> {
    // For small lists, use positional args to avoid process stdin overhead.
    if paths.len() <= 8 {
        let output = std::process::Command::new("git")
            .arg("check-ignore")
            .args(paths)
            .current_dir(root)
            .output()
            .context("failed to execute git check-ignore")?;
        return Ok(stdout_lines(&output.stdout).into_iter().collect());
    }

    // For large lists, pipe via stdin in a separate thread to avoid deadlock.
    use std::io::Write;
    let mut child = std::process::Command::new("git")
        .args(["check-ignore", "--stdin"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git check-ignore")?;

    let stdin = child.stdin.take().unwrap();
    let paths_owned: Vec<String> = paths.to_vec();
    std::thread::spawn(move || {
        let mut stdin = stdin;
        for path in &paths_owned {
            let _ = writeln!(stdin, "{}", path);
        }
    });

    let output = child.wait_with_output()?;
    Ok(stdout_lines(&output.stdout).into_iter().collect())
}

/// Extract values for a given config key.
pub fn config_values<'a>(config: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    config
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

/// Recursively copy a file or directory from `src` to `dst`.
pub fn copy_recursive(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)
            .with_context(|| format!("failed to create dir: {}", dst.display()))?;
        for entry in fs::read_dir(src)
            .with_context(|| format!("failed to read dir: {}", src.display()))?
        {
            let entry = entry?;
            let entry_dst = dst.join(entry.file_name());
            copy_recursive(&entry.path(), &entry_dst)?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)
            .with_context(|| format!("failed to copy {} -> {}", src.display(), dst.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let tmp = TempDir::new().expect("failed to create tempdir");
        let status = Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(tmp.path())
            .status()
            .expect("failed to run git init");
        assert!(status.success(), "git init should succeed");
        tmp
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
        assert_eq!(resolve_tool(&config, "ai"), "claude");
        assert_eq!(resolve_tool(&config, "editor"), "nvim");
    }

    #[test]
    fn resolve_tool_from_config() {
        let config = vec![
            ("waku.command.ai".to_string(), "aider".to_string()),
            ("waku.command.editor".to_string(), "vim".to_string()),
        ];
        assert_eq!(resolve_tool(&config, "ai"), "aider");
        assert_eq!(resolve_tool(&config, "editor"), "vim");
    }

    #[test]
    fn resolve_tool_command_splits_configured_arguments() {
        let config = vec![(
            "waku.command.ai".to_string(),
            "claude --resume --model sonnet".to_string(),
        )];
        let (program, args) = resolve_tool_command(&config, "ai").unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--resume", "--model", "sonnet"]);
    }

    #[test]
    fn resolve_tool_command_preserves_quoted_arguments() {
        let config = vec![(
            "waku.command.ai".to_string(),
            "claude --append \"hello world\"".to_string(),
        )];
        let (program, args) = resolve_tool_command(&config, "ai").unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--append", "hello world"]);
    }

    #[test]
    fn resolve_tool_in_reads_configured_tool_from_repo_root() {
        let repo = init_repo();
        let status = Command::new("git")
            .args(["config", "waku.command.editor", "hx"])
            .current_dir(repo.path())
            .status()
            .expect("failed to run git config");
        assert!(status.success(), "git config should succeed");

        let tool = resolve_tool_in(repo.path(), "editor").unwrap();

        assert_eq!(tool, "hx");
    }

    #[test]
    fn resolve_tool_command_in_splits_configured_arguments_from_repo_root() {
        let repo = init_repo();
        let status = Command::new("git")
            .args(["config", "waku.command.ai", "claude --resume --model sonnet"])
            .current_dir(repo.path())
            .status()
            .expect("failed to run git config");
        assert!(status.success(), "git config should succeed");

        let (program, args) = resolve_tool_command_in(repo.path(), "ai").unwrap();

        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--resume", "--model", "sonnet"]);
    }

    #[test]
    fn config_values_filters_by_key() {
        let config = vec![
            ("waku.link.include".to_string(), "node_modules".to_string()),
            ("waku.copy.include".to_string(), ".env".to_string()),
            ("waku.link.include".to_string(), ".direnv".to_string()),
        ];
        assert_eq!(config_values(&config, "waku.link.include"), vec!["node_modules", ".direnv"]);
        assert_eq!(config_values(&config, "waku.copy.include"), vec![".env"]);
    }

    #[test]
    fn config_values_returns_empty_for_missing_key() {
        let config = vec![
            ("waku.link.include".to_string(), "node_modules".to_string()),
        ];
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
    fn spinner_template_has_dots_after_message() {
        let msg_pos = SPINNER_TEMPLATE.find("{msg").expect("{msg} should exist in template");
        let spinner_pos = SPINNER_TEMPLATE.find("{spinner").expect("{spinner} should exist in template");
        assert!(
            spinner_pos > msg_pos,
            "{{spinner}} (dots) must appear after {{msg}} in template: {SPINNER_TEMPLATE}"
        );
    }
}
