use std::path::Path;

use anyhow::{Context, Result};
use console::{truncate_str, Key, Term};

use crate::git;

#[derive(Clone, Copy)]
pub(super) enum View {
    Diff,
    Log,
    Status,
}

fn load(path: &Path, refs: &[String], view: View) -> Result<String> {
    match view {
        View::Diff => git::git_output_in(
            path,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                refs.first().context("diff base unavailable")?,
                "--",
            ],
        ),
        View::Log => {
            let mut args = vec![
                "log",
                "--no-color",
                "--no-decorate",
                "--oneline",
                "HEAD",
                "--not",
            ];
            args.extend(refs.iter().map(String::as_str));
            args.push("--");
            git::git_output_in(path, &args)
        }
        View::Status => git::git_output_in(
            path,
            &[
                "status",
                "--short",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        ),
    }
}

fn display_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if ch == '\t' {
                ' '
            } else if ch.is_control() {
                '�'
            } else {
                ch
            }
        })
        .collect()
}

pub(super) fn show(term: &Term, path: &Path, refs: &[String], view: View) -> Result<()> {
    let label = match view {
        View::Diff => format!(
            "diff vs {} (working tree)",
            refs.first().map(String::as_str).unwrap_or("?")
        ),
        View::Log => "commits absent from base refs".to_string(),
        View::Status => "status".to_string(),
    };
    let output = load(path, refs, view).unwrap_or_else(|error| format!("{error:#}"));
    let lines: Vec<_> = if output.is_empty() {
        vec!["(no changes)".to_string()]
    } else {
        output.lines().map(display_line).collect()
    };
    let title = display_line(&format!("{label} · {}", path.display()));
    let mut offset = 0usize;
    loop {
        let (rows, cols) = term.size();
        let width = usize::from(cols).saturating_sub(1).max(1);
        let capacity = usize::from(rows).saturating_sub(3).max(1);
        let last = lines.len().saturating_sub(capacity);
        offset = offset.min(last);
        let end = (offset + capacity).min(lines.len());
        term.write_line(&truncate_str(&title, width, "…"))?;
        for line in &lines[offset..end] {
            term.write_line(&truncate_str(line, width, "…"))?;
        }
        let footer = format!(
            "{}–{end}/{} · ↑↓/PgUp/PgDn scroll · Enter/q back",
            offset + 1,
            lines.len()
        );
        term.write_line(&truncate_str(&footer, width, "…"))?;
        let key = term.read_key();
        term.clear_last_lines(end - offset + 2)?;
        match key? {
            Key::Escape | Key::Enter | Key::Char('q') => return Ok(()),
            Key::CtrlC => anyhow::bail!("cancelled; no selected deletions were applied"),
            Key::ArrowUp | Key::Char('k') => offset = offset.saturating_sub(1),
            Key::ArrowDown | Key::Char('j') => offset = (offset + 1).min(last),
            Key::PageUp => offset = offset.saturating_sub(capacity),
            Key::PageDown => offset = (offset + capacity).min(last),
            Key::Home => offset = 0,
            Key::End => offset = last,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git;
    use std::fs;

    #[test]
    fn detail_views_show_commits_edits_and_untracked_files_without_changing_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git::git_in(path, &args).unwrap();
        }
        fs::write(path.join("tracked.txt"), "base\n").unwrap();
        git::git_in(path, &["add", "."]).unwrap();
        git::git_in(path, &["commit", "-m", "initial"]).unwrap();
        git::git_in(path, &["switch", "-c", "topic"]).unwrap();
        fs::write(path.join("tracked.txt"), "committed\n").unwrap();
        git::git_in(path, &["commit", "-am", "topic work"]).unwrap();
        fs::write(path.join("tracked.txt"), "edited\n").unwrap();
        fs::write(path.join("untracked.txt"), "untracked\n").unwrap();
        let before = git::git_output_in(path, &["status", "--porcelain"]).unwrap();
        let refs = vec!["main".into()];
        let diff = load(path, &refs, View::Diff).unwrap();
        assert!(diff.contains("-base") && diff.contains("+edited"), "{diff}");
        let log = load(path, &refs, View::Log).unwrap();
        assert!(
            log.contains("topic work") && !log.contains("initial"),
            "{log}"
        );
        assert!(load(path, &refs, View::Status)
            .unwrap()
            .contains("untracked.txt"));
        assert_eq!(
            before,
            git::git_output_in(path, &["status", "--porcelain"]).unwrap()
        );
    }

    #[test]
    fn file_content_cannot_emit_terminal_control_sequences() {
        let rendered = display_line("\x1b]52;c;payload\x07\ttext\r");
        assert!(!rendered.chars().any(char::is_control), "{rendered:?}");
    }
}
