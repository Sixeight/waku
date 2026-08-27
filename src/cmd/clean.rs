use std::collections::{HashMap, HashSet};

use anyhow::Result;
use console::{measure_text_width, style, truncate_str, Key, Term};

use super::{cleanup_empty_dirs, print_warning, spinner};
use crate::{git, worktree};

struct WorktreeAnnotations<'a> {
    dirty: &'a HashSet<String>,
    unchanged: &'a HashSet<String>,
    gone: &'a HashSet<String>,
    commits: &'a HashMap<String, CommitInfo>,
    unique_commits: &'a HashMap<String, usize>,
    force: bool,
}

struct CommitInfo {
    updated: String,
    subject: String,
}

struct TableLayout {
    branch: usize,
    reason: usize,
    files: usize,
    commits: usize,
    updated: usize,
    row_width: usize,
    show_details: bool,
}

pub fn run(dry_run: bool, yes: bool, force: bool) -> Result<()> {
    let root = worktree::repo_root()?;

    // Fast local operations first
    let main_branch = git::git_output_in(&root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let upstream_ref = format!("{main_branch}@{{upstream}}");
    let upstream = git::git_output_in(&root, &["rev-parse", "--abbrev-ref", &upstream_ref]).ok();

    // Slow operations in parallel (fetch is network I/O)
    let sp = spinner("Fetching remote".into());
    let (_fetch_result, worktrees) = std::thread::scope(|s| {
        let fetch_handle = s.spawn(|| git::git_output_in(&root, &["fetch", "--prune"]));
        let wt_handle = s.spawn(|| git::worktree_list(&root));
        (fetch_handle.join().unwrap(), wt_handle.join().unwrap())
    });
    let worktrees = worktrees?;
    sp.finish_and_clear();
    eprintln!("  {} Fetched remote", style("✔").green());

    let mut check_refs = vec![main_branch.clone()];
    if let Some(ref u) = upstream {
        check_refs.push(u.clone());
    }

    // Run `git branch --merged` for each ref in parallel
    let merged_branches: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = check_refs
            .iter()
            .map(|check_ref| {
                s.spawn(|| git::git_output_in(&root, &["branch", "--merged", check_ref]))
            })
            .collect();
        let mut merged = Vec::new();
        for handle in handles {
            if let Ok(output) = handle.join().unwrap() {
                for b in parse_branch_list(&output, &main_branch) {
                    if !merged.contains(&b) {
                        merged.push(b);
                    }
                }
            }
        }
        merged
    });

    // Separate branched and detached worktrees
    let root_str = root.to_string_lossy().to_string();
    let mut detached: Vec<String> = Vec::new();
    let candidates: Vec<_> = worktrees
        .iter()
        .filter(|(path, _)| path != &root_str)
        .filter_map(|(path, wt_branch)| {
            let branch = match wt_branch.as_ref() {
                Some(b) => b,
                None => {
                    detached.push(path.clone());
                    return None;
                }
            };
            if merged_branches.iter().any(|b| b == branch) {
                return Some((path.clone(), branch.clone(), true));
            }
            Some((path.clone(), branch.clone(), false))
        })
        .collect();

    // Run is_merge_noop in parallel for unresolved candidates
    let merged_candidates: Vec<(String, String)> = std::thread::scope(|s| {
        let handles: Vec<_> = candidates
            .iter()
            .filter(|(_, _, already_merged)| !already_merged)
            .map(|(path, branch, _)| {
                s.spawn(|| {
                    let merged = check_refs
                        .iter()
                        .any(|r| git::is_merge_noop(&root, r, branch).unwrap_or(false));
                    (path.clone(), branch.clone(), merged)
                })
            })
            .collect();
        let mut result: Vec<_> = candidates
            .iter()
            .filter(|(_, _, already_merged)| *already_merged)
            .map(|(p, b, _)| (p.clone(), b.clone()))
            .collect();
        for handle in handles {
            let (path, branch, merged) = handle.join().unwrap();
            if merged {
                result.push((path, branch));
            }
        }
        result
    });

    // Filter out branches that haven't diverged from their fork point.
    // A branch with no unique commits since creation is "not yet started", not "merged".
    // These unchanged worktrees are still included as candidates but marked separately.
    let first_parents = git::first_parent_commits(&root, &main_branch);
    let mut unchanged_set: HashSet<String> = HashSet::new();
    let mut to_remove: Vec<(String, Option<String>)> = Vec::new();
    for (path, branch) in merged_candidates {
        if git::has_branch_diverged(&root, &first_parents, &branch) {
            to_remove.push((path, Some(branch)));
        } else {
            unchanged_set.insert(path.clone());
            to_remove.push((path, Some(branch)));
        }
    }

    // Detached worktrees have no branch — always candidates for removal
    for path in &detached {
        to_remove.push((path.clone(), None));
    }

    // Detect branches whose upstream tracking ref is gone (closed PR / deleted remote branch).
    // Only check worktrees that haven't already been collected as merged/unchanged.
    let already_collected: HashSet<String> = to_remove.iter().map(|(p, _)| p.clone()).collect();
    let gone_candidates: Vec<_> = worktrees
        .iter()
        .filter(|(path, _)| path != &root_str && !already_collected.contains(path))
        .filter_map(|(path, branch)| branch.as_ref().map(|b| (path.clone(), b.clone())))
        .collect();

    let gone_set: HashSet<String> = std::thread::scope(|s| {
        let handles: Vec<_> = gone_candidates
            .iter()
            .map(|(path, branch)| {
                s.spawn(|| {
                    let gone = git::has_upstream_gone(&root, branch);
                    (path.clone(), gone)
                })
            })
            .collect();
        let mut set = HashSet::new();
        for h in handles {
            let (path, gone) = h.join().unwrap();
            if gone {
                set.insert(path);
            }
        }
        set
    });

    for (path, branch) in &gone_candidates {
        if gone_set.contains(path) {
            to_remove.push((path.clone(), Some(branch.clone())));
        }
    }

    // Dirty check + commit info in parallel
    let waku_config = git::config_get_regexp_in(&root, r"^waku\.")?;
    let (dirty_set, commit_info, unique_commits): (
        HashSet<String>,
        HashMap<String, CommitInfo>,
        HashMap<String, usize>,
    ) = std::thread::scope(|s| {
        let handles: Vec<_> = to_remove
            .iter()
            .map(|(path, branch)| {
                let config_ref = &waku_config;
                let target_refs = &check_refs;
                let is_unchanged = unchanged_set.contains(path);
                let needs_unique_count = gone_set.contains(path) || branch.is_none();
                s.spawn(move || {
                    let wt_path = std::path::Path::new(path);
                    let is_dirty = if yes && force && !dry_run {
                        false
                    } else {
                        super::remove::is_worktree_dirty(wt_path, config_ref)
                    };
                    let commit = git::last_commit_info(wt_path);
                    let unique_count = if is_unchanged {
                        Some(0)
                    } else if needs_unique_count {
                        git::unique_commit_count(wt_path, target_refs)
                    } else {
                        None
                    };
                    (path.clone(), is_dirty, commit, unique_count)
                })
            })
            .collect();
        let mut dirty = HashSet::new();
        let mut commits = HashMap::new();
        let mut unique_commits = HashMap::new();
        for h in handles {
            let (path, is_dirty, commit, unique_count) = h.join().unwrap();
            if is_dirty {
                dirty.insert(path.clone());
            }
            if let Some((date, subject)) = commit {
                let subject = truncate_str(&subject, 50, "…");
                commits.insert(
                    path.clone(),
                    CommitInfo {
                        updated: date,
                        subject: subject.to_string(),
                    },
                );
            }
            if let Some(count) = unique_count {
                unique_commits.insert(path, count);
            }
        }
        (dirty, commits, unique_commits)
    });

    // Summary of found worktrees
    let unchanged_count = unchanged_set.len();
    let closed_count = gone_set.len();
    let merged_count = to_remove
        .iter()
        .filter(|(path, b)| {
            b.is_some() && !unchanged_set.contains(path) && !gone_set.contains(path)
        })
        .count();
    let detached_count = detached.len();
    let mut found_parts = Vec::new();
    if merged_count > 0 {
        found_parts.push(format!("{merged_count} merged"));
    }
    if closed_count > 0 {
        found_parts.push(format!("{closed_count} closed"));
    }
    if detached_count > 0 {
        found_parts.push(format!("{detached_count} detached"));
    }
    if unchanged_count > 0 {
        found_parts.push(format!("{unchanged_count} unchanged"));
    }
    if !found_parts.is_empty() {
        let total = merged_count + closed_count + detached_count + unchanged_count;
        let wt_word = if total == 1 { "worktree" } else { "worktrees" };
        eprintln!(
            "  {} Found {} {wt_word}",
            style("✔").green(),
            found_parts.join(", "),
        );
    }

    if to_remove.is_empty() {
        println!("No worktrees to clean.");
        return Ok(());
    }

    let annotations = WorktreeAnnotations {
        dirty: &dirty_set,
        unchanged: &unchanged_set,
        gone: &gone_set,
        commits: &commit_info,
        unique_commits: &unique_commits,
        force,
    };

    if dry_run {
        let layout = table_layout(
            &to_remove,
            &annotations,
            usize::from(Term::stdout().size().1).saturating_sub(4),
        );
        println!("{}", candidate_title(layout.row_width));
        println!("  {}", table_header(&layout));
        for (path, branch) in &to_remove {
            let checked = initially_checked(path, branch.as_deref(), &annotations);
            let row = worktree_row(path, branch.as_deref(), &annotations, &layout, checked);
            println!("  {row}");
        }
        return Ok(());
    }

    let selected = if yes {
        for (path, branch) in &to_remove {
            if dirty_set.contains(path) && !force {
                let name = display_name(path, branch.as_deref());
                eprintln!("  {} Skipped {} (dirty)", style("⚠").yellow(), name,);
                let err = anyhow::anyhow!(
                    "'{path}' contains modified or untracked files, use --force to delete"
                );
                print_warning(&format!("skipped worktree '{name}'"), &err);
            }
        }
        to_remove
            .iter()
            .filter(|(path, branch)| initially_checked(path, branch.as_deref(), &annotations))
            .cloned()
            .collect()
    } else {
        let chosen = select_worktrees(&to_remove, &annotations)?;
        if chosen.is_empty() {
            println!("Aborted.");
            return Ok(());
        }
        chosen
    };

    // `git worktree remove` takes the repository lock, so surfacing per-worktree
    // progress is more useful than spawning concurrent removals here.
    let total = selected.len();
    for (index, (path, branch)) in selected.into_iter().enumerate() {
        let name = display_name(&path, branch.as_deref());
        let progress = format!("{name} ({}/{total})", index + 1);
        let sp = spinner(format!("Removing {progress}"));
        let result = git::git_output_in(&root, &["worktree", "remove", "--force", &path]);
        sp.finish_and_clear();

        match result {
            Ok(_) => {
                if let Some(ref b) = branch {
                    if let Err(e) = git::git_output_in(&root, &["branch", "-D", b]) {
                        print_warning(&format!("failed to delete branch '{b}'"), &e);
                    }
                }
                eprintln!("  {} Removed {}", style("✔").green(), progress);
            }
            Err(e) => {
                eprintln!(
                    "  {} Failed to remove {}",
                    style("✘").red().bold(),
                    progress
                );
                let err = anyhow::anyhow!("failed to remove worktree: {e}");
                print_warning(&format!("failed to remove worktree '{name}'"), &err);
            }
        }
    }

    // Clean up empty directories in worktrees base
    let base = worktree::worktrees_base_with_config(&root, &waku_config)?;
    if base.exists() {
        cleanup_empty_dirs(&base)?;
    }

    Ok(())
}

fn select_worktrees(
    items: &[(String, Option<String>)],
    ann: &WorktreeAnnotations,
) -> Result<Vec<(String, Option<String>)>> {
    let term = Term::stderr();
    let count = items.len();
    let mut checked: Vec<bool> = items
        .iter()
        .map(|(path, branch)| initially_checked(path, branch.as_deref(), ann))
        .collect();
    let mut cursor = count;
    let lines = count + 3;

    term.hide_cursor()?;
    draw_selector(&term, items, &checked, ann, cursor);

    let result = loop {
        match term.read_key()? {
            Key::ArrowUp | Key::Char('k') if cursor > 0 => cursor -= 1,
            Key::ArrowDown | Key::Char('j') if cursor < count => cursor += 1,
            Key::Char(' ') if cursor < count => checked[cursor] = !checked[cursor],
            Key::Enter if cursor == count => {
                term.clear_last_lines(lines)?;
                break Ok(items
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| checked[*i])
                    .map(|(_, item)| item.clone())
                    .collect());
            }
            Key::Escape | Key::Char('q') => {
                term.clear_last_lines(lines)?;
                break Ok(vec![]);
            }
            _ => continue,
        }
        term.clear_last_lines(lines)?;
        draw_selector(&term, items, &checked, ann, cursor);
    };
    term.show_cursor()?;
    result
}

fn draw_selector(
    term: &Term,
    items: &[(String, Option<String>)],
    checked: &[bool],
    ann: &WorktreeAnnotations,
    cursor: usize,
) {
    let layout = table_layout(items, ann, usize::from(term.size().1).saturating_sub(4));
    let _ = term.write_line(&candidate_title(layout.row_width));
    let _ = term.write_line(&format!("    {}", table_header(&layout)));
    for (i, (path, branch)) in items.iter().enumerate() {
        let row = worktree_row(path, branch.as_deref(), ann, &layout, checked[i]);
        if cursor == i {
            let _ = term.write_line(&format!("  {} {row}", style("▸").bold()));
        } else {
            let _ = term.write_line(&format!("    {row}"));
        }
    }
    if cursor == items.len() {
        let _ = term.write_line(&format!("  {} {}", style("▸").bold(), style("run").bold()));
    } else {
        let _ = term.write_line(&format!("    {}", style("run").dim()));
    }
}

fn display_name(path: &str, branch: Option<&str>) -> String {
    branch.map(|b| b.to_string()).unwrap_or_else(|| {
        std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    })
}

fn initially_checked(path: &str, branch: Option<&str>, ann: &WorktreeAnnotations) -> bool {
    (!ann.dirty.contains(path) || ann.force)
        && !ann.unchanged.contains(path)
        && (worktree_reason(path, branch, ann) == "merged"
            || ann.unique_commits.get(path).copied() == Some(0))
}

fn table_layout(
    items: &[(String, Option<String>)],
    ann: &WorktreeAnnotations,
    row_width: usize,
) -> TableLayout {
    let mut layout = TableLayout {
        branch: measure_text_width("BRANCH"),
        reason: measure_text_width("REASON"),
        files: measure_text_width("FILES"),
        commits: measure_text_width("COMMITS"),
        updated: measure_text_width("UPDATED"),
        row_width: row_width.max(1),
        show_details: row_width >= 72,
    };
    for (path, branch) in items {
        let branch = branch.as_deref();
        layout.branch = layout
            .branch
            .max(measure_text_width(&display_name(path, branch)));
        layout.reason = layout
            .reason
            .max(measure_text_width(worktree_reason(path, branch, ann)));
        layout.commits = layout
            .commits
            .max(measure_text_width(&commit_status(path, branch, ann)));
        if let Some(commit) = ann.commits.get(path) {
            layout.updated = layout.updated.max(measure_text_width(&commit.updated));
        }
    }
    let (fixed_width, subject_reserve) = if layout.show_details {
        let subject = ann
            .commits
            .values()
            .map(|commit| measure_text_width(&commit.subject))
            .max()
            .unwrap_or(1)
            .min(12)
            .max(measure_text_width("SUBJECT"));
        (
            15 + layout.reason + layout.files + layout.commits + layout.updated,
            subject,
        )
    } else {
        (11 + layout.reason + layout.files + layout.commits, 0)
    };
    let branch_budget = layout
        .row_width
        .saturating_sub(fixed_width + subject_reserve)
        .max(1);
    layout.branch = layout.branch.min(branch_budget);
    layout
}

fn worktree_reason(path: &str, branch: Option<&str>, ann: &WorktreeAnnotations) -> &'static str {
    if ann.gone.contains(path) {
        "closed"
    } else if ann.unchanged.contains(path) {
        "unchanged"
    } else if branch.is_none() {
        "detached"
    } else {
        "merged"
    }
}

fn commit_status(path: &str, branch: Option<&str>, ann: &WorktreeAnnotations) -> String {
    if worktree_reason(path, branch, ann) == "merged" {
        "merged".to_string()
    } else {
        ann.unique_commits
            .get(path)
            .map(|count| format!("{count} unique"))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

fn padded(value: &str, width: usize) -> String {
    let value = truncate_str(value, width.max(1), "…");
    let padding = " ".repeat(width.saturating_sub(measure_text_width(&value)));
    format!("{value}{padding}")
}

fn candidate_title(width: usize) -> String {
    truncate_str(
        "Clean candidates ([✔] selected by default):",
        width.max(1),
        "…",
    )
    .into_owned()
}

fn table_header(layout: &TableLayout) -> String {
    let header = if layout.show_details {
        format!(
            "DEL  {}  {}  {}  {}  {}  SUBJECT",
            padded("BRANCH", layout.branch),
            padded("REASON", layout.reason),
            padded("FILES", layout.files),
            padded("COMMITS", layout.commits),
            padded("UPDATED", layout.updated),
        )
    } else {
        format!(
            "DEL  {}  {}  {}  {}",
            padded("BRANCH", layout.branch),
            padded("REASON", layout.reason),
            padded("FILES", layout.files),
            padded("COMMITS", layout.commits),
        )
    };
    truncate_str(&header, layout.row_width, "…").into_owned()
}

fn worktree_row(
    path: &str,
    branch: Option<&str>,
    ann: &WorktreeAnnotations,
    layout: &TableLayout,
    checked: bool,
) -> String {
    let mark = if checked { "✔" } else { " " };
    let name = display_name(path, branch);
    let visible_name = truncate_str(&name, layout.branch, "…");
    let styled_name = if branch.is_some() {
        style(&visible_name).bold().to_string()
    } else {
        visible_name.to_string()
    };
    let name_padding = " ".repeat(
        layout
            .branch
            .saturating_sub(measure_text_width(&visible_name)),
    );
    let reason = worktree_reason(path, branch, ann);
    let files = if ann.dirty.contains(path) {
        "dirty"
    } else {
        "clean"
    };
    let commits = commit_status(path, branch, ann);
    let (updated, subject) = ann
        .commits
        .get(path)
        .map(|commit| (commit.updated.as_str(), commit.subject.as_str()))
        .unwrap_or(("—", "—"));
    let row = if layout.show_details {
        format!(
            "[{mark}]  {styled_name}{name_padding}  {}  {}  {}  {}  {subject}",
            padded(reason, layout.reason),
            padded(files, layout.files),
            padded(&commits, layout.commits),
            padded(updated, layout.updated),
        )
    } else {
        format!(
            "[{mark}]  {styled_name}{name_padding}  {}  {}  {}",
            padded(reason, layout.reason),
            padded(files, layout.files),
            padded(&commits, layout.commits),
        )
    };
    truncate_str(&row, layout.row_width, "…").into_owned()
}

fn parse_branch_list(output: &str, exclude: &str) -> Vec<String> {
    output
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches("* ")
                .trim_start_matches("+ ")
                .to_string()
        })
        .filter(|b| b != exclude && !b.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use console::strip_ansi_codes;

    #[test]
    fn worktree_rows_align_decision_columns_by_display_width() {
        let merged_path = "/tmp/worktrees/short";
        let closed_path = "/tmp/worktrees/日本語";
        let items = vec![
            (merged_path.to_string(), Some("short".to_string())),
            (closed_path.to_string(), Some("日本語".to_string())),
        ];
        let dirty = HashSet::from([merged_path.to_string()]);
        let unchanged = HashSet::new();
        let gone = HashSet::from([closed_path.to_string()]);
        let commits = HashMap::from([
            (
                merged_path.to_string(),
                CommitInfo {
                    updated: "4 seconds ago".to_string(),
                    subject: "fix short".to_string(),
                },
            ),
            (
                closed_path.to_string(),
                CommitInfo {
                    updated: "2 days ago".to_string(),
                    subject: "fix wide".to_string(),
                },
            ),
        ]);
        let unique_commits = HashMap::from([(closed_path.to_string(), 2)]);
        let annotations = WorktreeAnnotations {
            dirty: &dirty,
            unchanged: &unchanged,
            gone: &gone,
            commits: &commits,
            unique_commits: &unique_commits,
            force: false,
        };
        let layout = table_layout(&items, &annotations, 200);

        let header = table_header(&layout);
        let merged_row = worktree_row(merged_path, Some("short"), &annotations, &layout, false);
        let closed_row = worktree_row(closed_path, Some("日本語"), &annotations, &layout, false);
        let merged = strip_ansi_codes(&merged_row);
        let closed = strip_ansi_codes(&closed_row);

        assert_eq!(
            header,
            "DEL  BRANCH  REASON  FILES  COMMITS   UPDATED        SUBJECT"
        );
        assert_eq!(
            merged,
            "[ ]  short   merged  dirty  merged    4 seconds ago  fix short"
        );
        assert_eq!(
            closed,
            "[ ]  日本語  closed  clean  2 unique  2 days ago     fix wide"
        );

        let narrow_layout = table_layout(&items, &annotations, 50);
        let narrow_header = table_header(&narrow_layout);
        let narrow_row = worktree_row(
            closed_path,
            Some("日本語"),
            &annotations,
            &narrow_layout,
            false,
        );
        assert!(measure_text_width(&narrow_header) <= 50);
        assert!(measure_text_width(&narrow_row) <= 50);
        assert!(strip_ansi_codes(&narrow_row).contains("2 unique"));
    }

    #[test]
    fn closed_and_detached_with_unique_commits_are_not_initially_checked() {
        for (path, branch, gone, unique) in [
            ("/tmp/worktrees/closed", Some("closed"), true, Some(1)),
            ("/tmp/worktrees/detached", None, false, Some(1)),
            ("/tmp/worktrees/unknown", Some("unknown"), true, None),
        ] {
            let dirty = HashSet::new();
            let unchanged = HashSet::new();
            let gone = if gone {
                HashSet::from([path.to_string()])
            } else {
                HashSet::new()
            };
            let commits = HashMap::new();
            let unique_commits = unique
                .map(|count| HashMap::from([(path.to_string(), count)]))
                .unwrap_or_default();
            let annotations = WorktreeAnnotations {
                dirty: &dirty,
                unchanged: &unchanged,
                gone: &gone,
                commits: &commits,
                unique_commits: &unique_commits,
                force: false,
            };

            assert!(!initially_checked(path, branch, &annotations));
        }
    }

    #[test]
    fn closed_without_unique_commits_is_initially_checked() {
        let path = "/tmp/worktrees/closed";
        let dirty = HashSet::new();
        let unchanged = HashSet::new();
        let gone = HashSet::from([path.to_string()]);
        let commits = HashMap::new();
        let unique_commits = HashMap::from([(path.to_string(), 0)]);
        let annotations = WorktreeAnnotations {
            dirty: &dirty,
            unchanged: &unchanged,
            gone: &gone,
            commits: &commits,
            unique_commits: &unique_commits,
            force: false,
        };

        assert!(initially_checked(path, Some("closed"), &annotations));
    }
}
