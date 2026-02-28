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

use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::{git, worktree};

pub fn spinner(msg: String) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["   ", ".  ", ".. ", "...", "   "])
            .template("  {prefix} {msg:.dim}{spinner:.dim}")
            .unwrap(),
    );
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

/// Resolve the command for a tool from waku config, with defaults.
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

/// Collect files matching `.worktreeinclude` patterns that are also gitignored.
/// Returns relative paths from `root`.
pub fn collect_worktreeinclude_files(root: &Path) -> Result<Vec<PathBuf>> {
    let wti_path = root.join(".worktreeinclude");
    if !wti_path.exists() {
        return Ok(vec![]);
    }

    let wti_content = fs::read_to_string(&wti_path)
        .with_context(|| format!("failed to read {}", wti_path.display()))?;

    let candidates: Vec<&str> = wti_content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.trim_end_matches('/'))
        .filter(|name| root.join(name).exists())
        .collect();

    if candidates.is_empty() {
        return Ok(vec![]);
    }

    let output = std::process::Command::new("git")
        .arg("check-ignore")
        .args(&candidates)
        .current_dir(root)
        .output()
        .context("failed to execute git check-ignore")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ignored: HashSet<&str> = stdout.lines().collect();

    Ok(candidates
        .into_iter()
        .filter(|name| ignored.contains(name))
        .map(PathBuf::from)
        .collect())
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
    use super::*;

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
}
