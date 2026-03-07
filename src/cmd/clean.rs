use std::collections::{HashMap, HashSet};

use anyhow::Result;
use console::{style, truncate_str, Key, Term};

use super::{cleanup_empty_dirs, print_warning, spinner};
use crate::{git, worktree};

struct WorktreeAnnotations<'a> {
    dirty: &'a HashSet<String>,
    unchanged: &'a HashSet<String>,
    gone: &'a HashSet<String>,
    commits: &'a HashMap<String, String>,
}

pub fn run(dry_run: bool, yes: bool, force: bool) -> Result<()> {
    let root = worktree::repo_root()?;

    // Fast local operations first
    let main_branch = git::git_output_in(&root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let upstream_ref = format!("{main_branch}@{{upstream}}");
    let upstream =
        git::git_output_in(&root, &["rev-parse", "--abbrev-ref", &upstream_ref]).ok();

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
    let (dirty_set, commit_info): (HashSet<String>, HashMap<String, String>) =
        std::thread::scope(|s| {
            let handles: Vec<_> = to_remove
                .iter()
                .map(|(path, _)| {
                    let config_ref = &waku_config;
                    s.spawn(move || {
                        let wt_path = std::path::Path::new(path);
                        let is_dirty =
                            !force && super::remove::is_worktree_dirty(wt_path, config_ref);
                        let commit = git::last_commit_info(wt_path);
                        (path.clone(), is_dirty, commit)
                    })
                })
                .collect();
            let mut dirty = HashSet::new();
            let mut commits = HashMap::new();
            for h in handles {
                let (path, is_dirty, commit) = h.join().unwrap();
                if is_dirty {
                    dirty.insert(path.clone());
                }
                if let Some((date, subject)) = commit {
                    let subject = truncate_str(&subject, 50, "…");
                    commits.insert(path, format!("{date}: {subject}"));
                }
            }
            (dirty, commits)
        });

    // Summary of found worktrees
    let unchanged_count = unchanged_set.len();
    let closed_count = gone_set.len();
    let merged_count = to_remove
        .iter()
        .filter(|(path, b)| {
            b.is_some()
                && !unchanged_set.contains(path)
                && !gone_set.contains(path)
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
        let wt_word = if total == 1 {
            "worktree"
        } else {
            "worktrees"
        };
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
    };

    if dry_run {
        println!("Worktrees to remove:");
        for (path, branch) in &to_remove {
            let label = worktree_label(path, branch.as_deref(), &annotations);
            println!("  {label}");
        }
        return Ok(());
    }

    let selected = if yes {
        for (path, branch) in &to_remove {
            if dirty_set.contains(path) {
                let name = display_name(path, branch.as_deref());
                eprintln!(
                    "  {} Skipped {} (dirty)",
                    style("⚠").yellow(),
                    name,
                );
                let err = anyhow::anyhow!(
                    "'{path}' contains modified or untracked files, use --force to delete"
                );
                print_warning(&format!("skipped worktree '{name}'"), &err);
            }
        }
        to_remove
            .iter()
            .filter(|(path, _)| !dirty_set.contains(path) && !unchanged_set.contains(path))
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

    // Remove worktrees in parallel
    let sp = {
        let label = if selected.len() == 1 {
            let (p, b) = &selected[0];
            format!("Removing {}", display_name(p, b.as_deref()))
        } else {
            format!("Removing {} worktrees", selected.len())
        };
        spinner(label)
    };
    let root_ref = &root;
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = selected
            .iter()
            .map(|(path, branch)| {
                s.spawn(move || {
                    let result =
                        git::git_output_in(root_ref, &["worktree", "remove", "--force", path]);
                    (path, branch, result)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    sp.finish_and_clear();

    for (path, branch, result) in results {
        let name = display_name(path, branch.as_deref());
        match result {
            Ok(_) => {
                if let Some(b) = branch {
                    if let Err(e) = git::git_output_in(&root, &["branch", "-D", b]) {
                        print_warning(&format!("failed to delete branch '{b}'"), &e);
                    }
                }
                eprintln!("  {} Removed {}", style("✔").green(), name);
            }
            Err(e) => {
                eprintln!(
                    "  {} Failed to remove {}",
                    style("✘").red().bold(),
                    name
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
        .map(|(path, _)| !ann.dirty.contains(path) && !ann.unchanged.contains(path))
        .collect();
    let mut cursor = count;
    let lines = count + 2;

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
    let _ = term.write_line("Worktrees to remove:");
    for (i, (path, branch)) in items.iter().enumerate() {
        let label = worktree_label(path, branch.as_deref(), ann);
        let mark = if checked[i] { "✔" } else { " " };
        if cursor == i {
            let _ = term.write_line(&format!(
                "  {} [{}] {}",
                style("▸").bold(),
                mark,
                style(&label).bold()
            ));
        } else {
            let _ = term.write_line(&format!("    [{}] {}", mark, label));
        }
    }
    if cursor == items.len() {
        let _ = term.write_line(&format!(
            "  {} {}",
            style("▸").bold(),
            style("run").bold()
        ));
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

fn worktree_label(
    path: &str,
    branch: Option<&str>,
    ann: &WorktreeAnnotations,
) -> String {
    let name = display_name(path, branch);
    let parts = worktree_label_parts(path, ann);
    if parts.is_empty() {
        name
    } else {
        format!("{name} ({})", parts.join(", "))
    }
}

fn worktree_label_parts(path: &str, ann: &WorktreeAnnotations) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if ann.dirty.contains(path) {
        parts.push("dirty".to_string());
    }
    if ann.gone.contains(path) {
        parts.push("closed".to_string());
    }
    if ann.unchanged.contains(path) {
        parts.push("no changes".to_string());
    }
    if let Some(info) = ann.commits.get(path) {
        parts.push(info.clone());
    }
    parts
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
