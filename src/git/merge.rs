use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, ensure, Context, Result};

use super::MergeTarget;

pub fn merge_noop_results(dir: &Path, target: &MergeTarget, sources: &[&str]) -> Result<Vec<bool>> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = String::new();
    for source in sources {
        for reference in [target.commit.as_str(), source] {
            ensure!(
                !reference.is_empty()
                    && !reference
                        .bytes()
                        .any(|byte| byte.is_ascii_whitespace() || byte == 0),
                "invalid merge reference"
            );
        }
        input.push_str(&target.commit);
        input.push(' ');
        input.push_str(source);
        input.push('\n');
    }
    let mut child = Command::new("git")
        .args([
            "merge-tree",
            "--write-tree",
            "--stdin",
            "--no-messages",
            "--name-only",
        ])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to execute batch merge-tree")?;
    let mut stdin = child.stdin.take().context("missing merge-tree input")?;
    let (output, written) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input.as_bytes()));
        let output = child.wait_with_output();
        let written = match writer.join() {
            Ok(result) => result,
            Err(error) => std::panic::resume_unwind(error),
        };
        (output, written)
    });
    let output = output.context("failed to read batch merge-tree output")?;
    if !output.status.success() {
        bail!(
            "batch merge-tree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    written.context("failed to write batch merge-tree input")?;
    parse_results(&output.stdout, &target.tree, sources.len())
}

fn parse_results(output: &[u8], tree: &str, count: usize) -> Result<Vec<bool>> {
    let mut fields = output.split(|byte| *byte == 0);
    let mut results = Vec::with_capacity(count);
    for _ in 0..count {
        let status = fields.next().context("missing merge status")?;
        let merged_tree = fields.next().context("missing merge tree")?;
        ensure!(!merged_tree.is_empty(), "empty merge tree");
        // --stdin succeeds even when a merge conflicts; each record carries its own status.
        let merged = match status {
            b"0" => false,
            b"1" => merged_tree == tree.as_bytes(),
            _ => bail!("invalid merge status"),
        };
        ensure!(
            fields.by_ref().any(|field| field.is_empty()),
            "unterminated merge record"
        );
        results.push(merged);
    }
    ensure!(
        fields.next() == Some(b"".as_slice()) && fields.next().is_none(),
        "unexpected merge output"
    );
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn supports_batch_merges(dir: &Path) -> bool {
        Command::new("git")
            .args([
                "merge-tree",
                "--write-tree",
                "--stdin",
                "--no-messages",
                "--name-only",
            ])
            .stdin(Stdio::null())
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn parses_clean_and_conflicted_results_without_treating_paths_as_statuses() {
        let output = [
            "1",
            "same",
            "",
            "0",
            "same",
            "path\nwith\ttabs",
            "1",
            "",
            "1",
            "other",
            "",
            "",
        ]
        .join("\0");
        assert_eq!(
            parse_results(output.as_bytes(), "same", 3).unwrap(),
            [true, false, false]
        );
    }

    #[test]
    fn rejects_truncated_extra_and_unrecognized_results() {
        for (output, count) in [
            ("1\0same\0", 1),
            ("1\0\0\0", 1),
            ("2\0same\0\0", 1),
            ("1\0same\0\0", 2),
            ("1\0same\0\x001\0same\0\0", 1),
        ] {
            assert!(parse_results(output.as_bytes(), "same", count).is_err());
        }
    }

    #[test]
    fn batch_matches_individual_checks_for_squash_conflicts_and_pending_changes() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-qb", "main"]);
        git(dir, &["commit", "--allow-empty", "-qm", "initial"]);
        if !supports_batch_merges(dir) {
            return;
        }
        git(dir, &["checkout", "-qb", "feature"]);
        fs::write(dir.join("conflict\nname.txt"), "feature\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "feature"]);
        git(dir, &["checkout", "-q", "main"]);
        fs::write(dir.join("conflict\nname.txt"), "feature\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "squash feature"]);
        git(dir, &["checkout", "-qb", "conflict", "feature"]);
        fs::write(dir.join("conflict\nname.txt"), "conflicting content\n").unwrap();
        git(dir, &["commit", "-qam", "conflict"]);
        git(dir, &["checkout", "-qb", "pending", "main"]);
        fs::write(dir.join("pending.txt"), "pending\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "pending"]);
        let target = crate::git::merge_target(dir, "main").unwrap();
        let sources = ["feature", "conflict", "pending", "main", "feature"];
        let head = git(dir, &["rev-parse", "HEAD"]);
        let expected: Vec<_> = sources
            .iter()
            .map(|source| crate::git::is_merge_noop_with_target(dir, &target, source).unwrap())
            .collect();
        assert_eq!(expected, [true, false, false, true, true]);
        assert_eq!(
            merge_noop_results(dir, &target, &sources).unwrap(),
            expected
        );
        assert_eq!(git(dir, &["rev-parse", "HEAD"]), head);
        assert!(git(dir, &["status", "--porcelain"]).is_empty());
        assert!(merge_noop_results(dir, &target, &[]).unwrap().is_empty());
        assert!(merge_noop_results(dir, &target, &["main\nmain main"]).is_err());
    }

    #[test]
    fn large_batch_drains_output_while_writing_input() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-qb", "main"]);
        git(dir, &["commit", "--allow-empty", "-qm", "initial"]);
        if !supports_batch_merges(dir) {
            return;
        }
        let target = crate::git::merge_target(dir, "main").unwrap();
        let sources = vec![target.commit.as_str(); 2048];
        assert_eq!(
            merge_noop_results(dir, &target, &sources).unwrap(),
            vec![true; sources.len()]
        );
    }
}
