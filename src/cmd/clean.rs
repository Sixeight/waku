use anyhow::Result;
use console::{style, Key, Term};

use super::{
    cleanup_empty_dirs, print_warning, remove_waku_copies, remove_waku_symlinks,
    remove_worktreeinclude_files, spinner,
};
use crate::{git, worktree};

pub fn run(dry_run: bool, yes: bool, force: bool) -> Result<()> {
    let root = worktree::repo_root()?;

    let sp = spinner("Checking merged worktrees".into());

    // Fast local operations first
    let main_branch = git::git_output_in(&root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let upstream_ref = format!("{main_branch}@{{upstream}}");
    let upstream =
        git::git_output_in(&root, &["rev-parse", "--abbrev-ref", &upstream_ref]).ok();

    // Slow operations in parallel (fetch is network I/O)
    let (_fetch_result, worktrees) = std::thread::scope(|s| {
        let fetch_handle = s.spawn(|| git::git_output_in(&root, &["fetch"]));
        let wt_handle = s.spawn(|| git::worktree_list(&root));
        (fetch_handle.join().unwrap(), wt_handle.join().unwrap())
    });
    let worktrees = worktrees?;

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

    // Filter worktrees that need is_merge_noop check
    let root_str = root.to_string_lossy().to_string();
    let candidates: Vec<_> = worktrees
        .iter()
        .filter_map(|(path, wt_branch)| {
            let branch = wt_branch.as_ref()?;
            if path == &root_str {
                return None;
            }
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
    // Load main's first-parent chain once, then O(1) lookup per branch.
    let first_parents = git::first_parent_commits(&root, &main_branch);
    let to_remove: Vec<(String, String)> = merged_candidates
        .into_iter()
        .filter(|(_, branch)| git::has_branch_diverged(&root, &first_parents, branch))
        .collect();

    sp.finish_and_clear();

    if to_remove.is_empty() {
        println!("No merged worktrees to clean.");
        return Ok(());
    }

    if dry_run {
        println!("Merged worktrees to remove:");
        for (_path, branch) in &to_remove {
            println!("  {branch}");
        }
        return Ok(());
    }

    let selected = if yes {
        to_remove.clone()
    } else {
        let chosen = select_worktrees(&to_remove)?;
        if chosen.is_empty() {
            println!("Aborted.");
            return Ok(());
        }
        chosen
    };

    // Phase 1: Remove symlinks and directories in parallel
    let sp = spinner("Removing worktrees".into());
    let results: Vec<(String, String, bool, Option<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = selected
            .iter()
            .map(|(path, branch)| {
                let root_ref = &root;
                s.spawn(move || {
                    let wt_path = std::path::Path::new(path);

                    // Remove waku symlinks, copies, and worktreeinclude files first
                    let _ = remove_waku_symlinks(wt_path);
                    let _ = remove_waku_copies(wt_path);
                    let _ = remove_worktreeinclude_files(wt_path, root_ref);

                    // Dirty check (non-force only)
                    if !force {
                        if let Ok(status) =
                            git::git_output_in(wt_path, &["status", "--porcelain"])
                        {
                            if !status.is_empty() {
                                return (
                                    path.clone(),
                                    branch.clone(),
                                    false,
                                    Some(format!(
                                        "'{path}' contains modified or untracked files, use --force to delete"
                                    )),
                                );
                            }
                        }
                    }

                    // Remove directory
                    match std::fs::remove_dir_all(wt_path) {
                        Ok(()) => (path.clone(), branch.clone(), true, None),
                        Err(e) => (
                            path.clone(),
                            branch.clone(),
                            false,
                            Some(format!("failed to remove directory: {e}")),
                        ),
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });
    sp.finish_and_clear();

    // Phase 2: Prune stale worktree entries (single git call)
    let _ = git::git_output_in(&root, &["worktree", "prune"]);

    // Phase 3: Delete branches and report results
    for (path, branch, success, warning) in &results {
        if let Some(msg) = warning {
            let err = anyhow::anyhow!("{msg}");
            print_warning(&format!("failed to remove worktree '{branch}'"), &err);
        }
        if *success {
            if let Err(e) = git::git_output_in(&root, &["branch", "-D", branch]) {
                print_warning(&format!("failed to delete branch '{branch}'"), &e);
            }
            println!("Removed: {} ({})", branch, path);
        }
    }

    // Clean up empty directories in worktrees base
    let waku_config = git::config_get_regexp_in(&root, r"^waku\.")?;
    let base = worktree::worktrees_base_with_config(&root, &waku_config)?;
    if base.exists() {
        cleanup_empty_dirs(&base)?;
    }

    Ok(())
}

fn select_worktrees(items: &[(String, String)]) -> Result<Vec<(String, String)>> {
    let term = Term::stderr();
    let count = items.len();
    let mut checked = vec![true; count];
    let mut cursor = count; // "実行" に初期フォーカス
    let lines = count + 2; // ヘッダ + items + 実行

    term.hide_cursor()?;
    draw_selector(&term, items, &checked, cursor);

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
        draw_selector(&term, items, &checked, cursor);
    };
    term.show_cursor()?;
    result
}

fn draw_selector(term: &Term, items: &[(String, String)], checked: &[bool], cursor: usize) {
    let _ = term.write_line("Worktrees to remove:");
    for (i, (_, branch)) in items.iter().enumerate() {
        let mark = if checked[i] { "✔" } else { " " };
        if cursor == i {
            let _ = term.write_line(&format!(
                "  {} [{}] {}",
                style("▸").bold(),
                mark,
                style(branch).bold()
            ));
        } else {
            let _ = term.write_line(&format!("    [{}] {}", mark, branch));
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
