pub mod clean;
pub mod config;
pub mod create;
pub mod open;
pub mod path;
pub mod remove;

use std::fs;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};

use crate::{config::Config, git};

pub const SPINNER_TEMPLATE: &str = "  {prefix} {msg:.dim}{spinner:.dim}";

fn spinner_style() -> ProgressStyle {
    ProgressStyle::default_spinner()
        .tick_strings(&["   ", ".  ", ".. ", "...", "   "])
        .template(SPINNER_TEMPLATE)
        .unwrap()
}

pub fn spinner(msg: String) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    if pb.is_hidden() {
        return pb;
    }
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
pub fn resolve_tool(config: &Config, tool: &str) -> String {
    config
        .last(&format!("waku.command.{tool}"))
        .map(str::to_string)
        .unwrap_or_else(|| match tool {
            "agent" => "claude".to_string(),
            _ => "nvim".to_string(),
        })
}

/// Resolve the command and configured arguments for a tool.
pub fn resolve_tool_command(config: &Config, tool: &str) -> Result<(String, Vec<String>)> {
    let command_line = resolve_tool(config, tool);
    parse_command_line(&command_line)
}

/// Resolve a command line from pre-loaded config, preferring a one-time override.
pub fn resolve_tool_command_with_override(
    config: &Config,
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
        let config = Config::empty();
        assert_eq!(resolve_tool(&config, "agent"), "claude");
        assert_eq!(resolve_tool(&config, "editor"), "nvim");
    }

    #[test]
    fn resolve_tool_from_config() {
        let config = Config::from_entries(vec![
            ("waku.command.agent".to_string(), "aider".to_string()),
            ("waku.command.editor".to_string(), "vim".to_string()),
        ]);
        assert_eq!(resolve_tool(&config, "agent"), "aider");
        assert_eq!(resolve_tool(&config, "editor"), "vim");
    }

    #[test]
    fn resolve_tool_prefers_last_value_like_git_config_get() {
        // git config --get-regexp lists system → global → local in order,
        // and `git config --get` returns the last (most specific) value.
        let config = Config::from_entries(vec![
            ("waku.command.agent".to_string(), "global-agent".to_string()),
            ("waku.command.agent".to_string(), "local-agent".to_string()),
        ]);
        assert_eq!(resolve_tool(&config, "agent"), "local-agent");
    }

    #[test]
    fn resolve_tool_command_splits_configured_arguments() {
        let config = Config::from_entries(vec![(
            "waku.command.agent".to_string(),
            "claude --resume --model sonnet".to_string(),
        )]);
        let (program, args) = resolve_tool_command(&config, "agent").unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--resume", "--model", "sonnet"]);
    }

    #[test]
    fn resolve_tool_command_preserves_quoted_arguments() {
        let config = Config::from_entries(vec![(
            "waku.command.agent".to_string(),
            "claude --append \"hello world\"".to_string(),
        )]);
        let (program, args) = resolve_tool_command(&config, "agent").unwrap();
        assert_eq!(program, "claude");
        assert_eq!(args, vec!["--append", "hello world"]);
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
