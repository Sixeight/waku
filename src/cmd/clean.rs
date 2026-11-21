use std::collections::{HashMap, HashSet};

use anyhow::Result;
use console::{style, truncate_str, Key, Term};

use super::{cleanup_empty_dirs, print_warning, spinner};
use crate::{config::Config, git, parallel, worktree};

mod agents;
mod details;
mod safety;
mod selector;
mod table;

use selector::{SelectorDisplay, SelectorUpdate};
use table::{
    candidate_title, display_name, table_header, table_layout, worktree_reason, worktree_row,
};

pub(super) struct WorktreeAnnotations<'a> {
    dirty: &'a HashSet<String>,
    unchanged: &'a HashSet<String>,
    gone: &'a HashSet<String>,
    commits: &'a HashMap<String, CommitInfo>,
    unique_commits: &'a HashMap<String, usize>,
    protected: &'a HashMap<String, &'static str>,
    force: bool,
}

pub(super) struct CommitInfo {
    updated: String,
    subject: String,
}

pub fn run(dry_run: bool, yes: bool, force: bool) -> Result<()> {
    let root = worktree::repo_root()?;

    // Run network fetch alongside local metadata, config, and agent inspection.
    let sp = spinner("Fetching remote".into());
    let (main_branch, upstream, _fetch_result, worktrees, config, agents) =
        std::thread::scope(|s| {
            let main_handle =
                s.spawn(|| git::git_output_in(&root, &["rev-parse", "--abbrev-ref", "HEAD"]));
            let upstream_handle = s.spawn(|| {
                git::git_output_in(&root, &["rev-parse", "--abbrev-ref", "HEAD@{upstream}"]).ok()
            });
            let fetch_handle = s.spawn(|| git::git_output_in(&root, &["fetch", "--prune"]));
            let wt_handle = s.spawn(|| git::worktree_details(&root));
            let config_handle = s.spawn(|| Config::load(&root));
            let agents_handle = s.spawn(agents::ActiveAgents::inspect);
            (
                main_handle.join().unwrap(),
                upstream_handle.join().unwrap(),
                fetch_handle.join().unwrap(),
                wt_handle.join().unwrap(),
                config_handle.join().unwrap(),
                agents_handle.join().unwrap(),
            )
        });
    let main_branch = main_branch?;
    let worktree_details = worktrees?;
    sp.finish_and_clear();
    eprintln!("  {} Fetched remote", style("✔").green());

    let root_str = root.to_string_lossy().to_string();
    if worktree_details
        .iter()
        .all(|worktree| worktree.path == root_str)
    {
        println!("No worktrees to clean.");
        return Ok(());
    }
    let waku_config = config?;
    let worktrees: Vec<_> = worktree_details
        .iter()
        .map(|worktree| (worktree.path.clone(), worktree.branch.clone()))
        .collect();
    let heads: HashMap<_, _> = worktree_details
        .iter()
        .filter_map(|worktree| {
            worktree
                .head
                .as_ref()
                .map(|head| (worktree.path.as_str(), head.as_str()))
        })
        .collect();

    let mut check_refs = vec![main_branch.clone()];
    if let Some(ref u) = upstream {
        check_refs.push(u.clone());
    }

    // Load all refs+trees and start first-parent walk in parallel with merge checks.
    let (
        ref_targets,
        first_parents,
        merge_targets,
        merged_branches,
        prefetched_counts,
        merged_results,
        detached,
        candidates,
    ) = std::thread::scope(|s| -> Result<_> {
        let first_parents_handle = s.spawn(|| git::first_parent_commits(&root, &main_branch));
        let ref_targets = git::ref_targets(&root).ok();

        let mut target_commits = HashSet::new();
        let mut merge_targets = Vec::new();
        check_refs.retain(|reference| {
            let target = ref_targets
                .as_ref()
                .and_then(|refs| {
                    refs.get(&format!("refs/heads/{reference}"))
                        .or_else(|| refs.get(&format!("refs/remotes/{reference}")))
                        .filter(|t| !t.tree.is_empty())
                        .cloned()
                })
                .or_else(|| git::merge_target(&root, reference).ok());
            let Some(target) = target else {
                return true;
            };
            if !target_commits.insert(target.commit.clone()) {
                return false;
            }
            merge_targets.push(target);
            true
        });

        let mut detached: Vec<String> = Vec::new();
        let branched_worktrees: Vec<(String, String)> = worktrees
            .iter()
            .filter(|(path, _)| path != &root_str)
            .filter_map(|(path, wt_branch)| match wt_branch.as_ref() {
                Some(b) => Some((path.clone(), b.clone())),
                None => {
                    detached.push(path.clone());
                    None
                }
            })
            .collect();

        // For a single merge target, one `for-each-ref --format=...ahead-behind`
        // determines both merged branches (ahead == 0) and unique commit counts.
        let mut prefetched_counts: Option<HashMap<String, usize>> = None;
        let merged_branches: HashSet<String> =
            match (check_refs.as_slice(), merge_targets.as_slice()) {
                ([_], [target]) if !branched_worktrees.is_empty() => {
                    let branch_names: Vec<&str> =
                        branched_worktrees.iter().map(|(_, b)| b.as_str()).collect();
                    match git::branch_ahead_counts(&root, &target.commit, &branch_names) {
                        Ok(ahead_map) => {
                            let merged: HashSet<String> = ahead_map
                                .iter()
                                .filter(|(branch, (_, ahead))| {
                                    *ahead == 0 && branch.as_str() != main_branch
                                })
                                .map(|(branch, _)| branch.clone())
                                .collect();
                            prefetched_counts = Some(ahead_map.into_values().collect());
                            merged
                        }
                        Err(_) => {
                            git::git_output_in(&root, &["branch", "--merged", &check_refs[0]])
                                .map(|output| parse_branch_list(&output, &main_branch))
                                .unwrap_or_default()
                                .into_iter()
                                .collect()
                        }
                    }
                }
                _ => parallel::map(&check_refs, |check_ref| {
                    git::git_output_in(&root, &["branch", "--merged", check_ref])
                        .map(|output| parse_branch_list(&output, &main_branch))
                        .unwrap_or_default()
                })
                .into_iter()
                .flatten()
                .collect(),
            };

        let candidates: Vec<_> = branched_worktrees
            .into_iter()
            .map(|(path, branch)| {
                let is_merged = merged_branches.contains(&branch);
                (path, branch, is_merged)
            })
            .collect();

        let unresolved: Vec<_> = candidates
            .iter()
            .filter(|(_, _, already_merged)| !already_merged)
            .collect();
        let mut merged_results = vec![false; unresolved.len()];
        for target in &merge_targets {
            let pending: Vec<_> = unresolved
                .iter()
                .enumerate()
                .filter(|(index, _)| !merged_results[*index])
                .collect();
            if pending.is_empty() {
                break;
            }
            let batches: Vec<_> = pending.chunks(8).collect();
            let groups: Result<Vec<Vec<bool>>> = parallel::map(&batches, |batch| {
                let sources: Vec<_> = batch
                    .iter()
                    .map(|(_, candidate)| candidate.1.as_str())
                    .collect();
                if sources.len() == 1 {
                    git::is_merge_noop_with_target(&root, target, sources[0])
                        .map(|result| vec![result])
                } else {
                    git::merge_noop_results(&root, target, &sources)
                }
            })
            .into_iter()
            .collect();
            let flags: Vec<_> = match groups {
                Ok(groups) => groups.into_iter().flatten().collect(),
                Err(_) => parallel::map(&pending, |(_, candidate)| {
                    git::is_merge_noop_with_target(&root, target, &candidate.1).unwrap_or(false)
                }),
            };
            for ((index, _), merged) in pending.into_iter().zip(flags) {
                merged_results[index] = merged;
            }
        }

        let first_parents = first_parents_handle.join().unwrap();
        Ok((
            ref_targets,
            first_parents,
            merge_targets,
            merged_branches,
            prefetched_counts,
            merged_results,
            detached,
            candidates,
        ))
    })?;
    let _ = (&merge_targets, &merged_branches);

    let unresolved: Vec<_> = candidates
        .iter()
        .filter(|(_, _, already_merged)| !already_merged)
        .collect();
    let merged_candidates: Vec<(String, String)> = candidates
        .iter()
        .filter(|(_, _, already_merged)| *already_merged)
        .map(|(p, b, _)| (p.clone(), b.clone()))
        .chain(
            unresolved
                .iter()
                .zip(merged_results)
                .filter(|(_, merged)| *merged)
                .map(|((path, branch, _), _)| (path.clone(), branch.clone())),
        )
        .collect();

    // Filter out branches that haven't diverged from their fork point.
    // A branch with no unique commits since creation is "not yet started", not "merged".
    // These unchanged worktrees are still included as candidates but marked separately.
    let mut unchanged_set: HashSet<String> = HashSet::new();
    let mut to_remove: Vec<(String, Option<String>)> = Vec::new();
    for (path, branch) in merged_candidates {
        let diverged = ref_targets
            .as_ref()
            .and_then(|refs| refs.get(&format!("refs/heads/{branch}")))
            .map(|target| !first_parents.contains(&target.commit))
            .unwrap_or_else(|| git::has_branch_diverged(&root, &first_parents, &branch));
        if !diverged {
            unchanged_set.insert(path.clone());
        }
        to_remove.push((path, Some(branch)));
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

    let branch_remotes: HashMap<_, _> = waku_config
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let gone_set: HashSet<String> = gone_candidates
        .iter()
        .filter(|(_, branch)| {
            if let Some(refs) = &ref_targets {
                branch_remotes
                    .get(format!("branch.{branch}.remote").as_str())
                    .is_some_and(|remote| !remote.trim().is_empty())
                    && !refs.contains_key(&format!("refs/remotes/origin/{branch}"))
            } else {
                git::has_upstream_gone(&root, branch)
            }
        })
        .map(|(path, _)| path.clone())
        .collect();

    for (path, branch) in &gone_candidates {
        if gone_set.contains(path) {
            to_remove.push((path.clone(), Some(branch.clone())));
        }
    }

    if to_remove.is_empty() {
        println!("No worktrees to clean.");
        return Ok(());
    }

    let need_commit_info = dry_run || !yes;
    let commit_ids: Vec<&str> = if need_commit_info {
        let unique: HashSet<_> = to_remove
            .iter()
            .filter_map(|(path, _)| heads.get(path.as_str()).copied())
            .collect();
        unique.into_iter().collect()
    } else {
        Vec::new()
    };

    // Dirty check + commit info + fallback unique commit counts in parallel
    let (dirty_set, commit_info, unique_commits) = std::thread::scope(|scope| {
        let commits_handle = scope.spawn(|| {
            if need_commit_info {
                git::commit_info(&root, &commit_ids).unwrap_or_default()
            } else {
                HashMap::new()
            }
        });
        let results = parallel::map(&to_remove, |(path, branch)| {
            let is_unchanged = unchanged_set.contains(path);
            let needs_unique_count = gone_set.contains(path) || branch.is_none();
            let wt_path = std::path::Path::new(path);
            let is_dirty = if yes && force && !dry_run {
                false
            } else {
                super::remove::is_worktree_dirty(wt_path, &waku_config)
            };
            let unique_count = if is_unchanged {
                Some(0)
            } else if needs_unique_count {
                heads
                    .get(path.as_str())
                    .and_then(|head| prefetched_counts.as_ref()?.get(*head))
                    .copied()
                    .or_else(|| git::unique_commit_count(wt_path, &check_refs))
            } else {
                None
            };
            (path.clone(), is_dirty, unique_count)
        });
        let commits_by_oid = commits_handle.join().unwrap();

        let mut dirty = HashSet::new();
        let mut commits = HashMap::new();
        let mut unique_commits = HashMap::new();
        for (path, is_dirty, unique_count) in results {
            if is_dirty {
                dirty.insert(path.clone());
            }
            if need_commit_info {
                let commit = heads
                    .get(path.as_str())
                    .and_then(|head| commits_by_oid.get(*head))
                    .cloned()
                    .or_else(|| git::last_commit_info(std::path::Path::new(&path)));
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

    if let Err(error) = &agents {
        print_warning("agent check unavailable; worktrees will be kept", error);
    }
    let protected: HashMap<_, _> = to_remove
        .iter()
        .filter_map(|(path, _)| match &agents {
            Ok(agents) if agents.uses(std::path::Path::new(path)) => {
                Some((path.clone(), "active agent"))
            }
            Err(_) => Some((path.clone(), "agent check unavailable")),
            _ => None,
        })
        .collect();
    let annotations = WorktreeAnnotations {
        dirty: &dirty_set,
        unchanged: &unchanged_set,
        gone: &gone_set,
        commits: &commit_info,
        unique_commits: &unique_commits,
        protected: &protected,
        force,
    };

    if dry_run {
        let stdout = Term::stdout();
        let row_width = if stdout.is_term() {
            usize::from(stdout.size().1).saturating_sub(4)
        } else {
            usize::MAX
        };
        let layout = table_layout(&to_remove, &annotations, row_width);
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
            if let Some(reason) = protected.get(path) {
                eprintln!(
                    "  {} Skipped {} ({reason})",
                    style("⚠").yellow(),
                    display_name(path, branch.as_deref())
                );
                continue;
            }
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
        let chosen = select_worktrees(&to_remove, &annotations, &check_refs)?;
        if chosen.is_empty() {
            println!("Aborted.");
            return Ok(());
        }
        chosen
    };

    // `git worktree remove` takes the repository lock, so keep removals
    // sequential. Non-trivial working trees are staged aside and deleted in
    // parallel across 4 workers while subsequent removals and branch deletion
    // proceed.
    let total = selected.len();
    let mut removed_worktrees = Vec::new();
    let mut failed = 0usize;
    let mut staged_dirs = Vec::new();

    let branch_failures = std::thread::scope(|scope| {
        let mut deleter_tx: Option<std::sync::mpsc::Sender<std::path::PathBuf>> = None;
        let mut deleter_handles = Vec::new();

        for (index, (path, branch)) in selected.into_iter().enumerate() {
            let name = display_name(&path, branch.as_deref());
            let progress = format!("{name} ({}/{total})", index + 1);
            let sp = spinner(format!("Removing {progress}"));
            let expected = worktree_details.iter().find(|item| item.path == path);
            let result = expected
                .ok_or_else(|| anyhow::anyhow!("worktree snapshot unavailable"))
                .and_then(|expected| {
                    agents::ActiveAgents::inspect()?.ensure_unused(std::path::Path::new(&path))?;
                    safety::remove_worktree_staged(
                        &root,
                        expected,
                        &waku_config,
                        force || dirty_set.contains(&path),
                    )
                });
            sp.finish_and_clear();

            match result {
                Ok(staged) => {
                    if let Some(staged_dir) = staged {
                        let tx = deleter_tx.get_or_insert_with(|| {
                            let (tx, rx) = std::sync::mpsc::channel::<std::path::PathBuf>();
                            let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
                            for _ in 0..4 {
                                let rx = std::sync::Arc::clone(&rx);
                                deleter_handles.push(scope.spawn(move || loop {
                                    let next = { rx.lock().unwrap().recv() };
                                    match next {
                                        Ok(path) => super::remove::remove_path_best_effort(&path),
                                        Err(_) => break,
                                    }
                                }));
                            }
                            tx
                        });
                        for task in super::remove::expand_removal_tasks(&staged_dir) {
                            let _ = tx.send(task);
                        }
                        staged_dirs.push(staged_dir);
                    }
                    removed_worktrees.push((progress, branch));
                }
                Err(e) => {
                    failed += 1;
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

        let branches_to_delete: Vec<String> = removed_worktrees
            .iter()
            .filter_map(|(_, branch)| branch.as_ref().cloned())
            .collect();
        let failures = if branches_to_delete.is_empty() {
            HashMap::new()
        } else {
            let sp = spinner(format!("Deleting {} branches", branches_to_delete.len()));
            let failures = delete_branches(&root, &branches_to_delete, ref_targets.as_ref());
            sp.finish_and_clear();
            failures
        };

        drop(deleter_tx);
        for handle in deleter_handles {
            handle.join().unwrap();
        }
        failures
    });

    for dir in staged_dirs {
        let _ = std::fs::remove_dir_all(dir);
    }

    let removed = removed_worktrees.len();
    for (progress, branch) in removed_worktrees {
        if let Some(branch) = branch {
            if let Some(failure) = branch_failures.get(&branch) {
                let (message, error) = match failure {
                    BranchDeleteFailure::Failed(error) => (
                        format!("failed to delete branch '{branch}'"),
                        error.as_str(),
                    ),
                    BranchDeleteFailure::Unverified(error) => (
                        format!("could not verify deletion of branch '{branch}'"),
                        error.as_str(),
                    ),
                };
                print_warning(&message, &anyhow::anyhow!("{error}"));
            }
        }
        eprintln!("  {} Removed {}", style("✔").green(), progress);
    }

    // Clean up empty directories in worktrees base
    let base = worktree::worktrees_base_with_config(&root, &waku_config)?;
    if base.exists() {
        cleanup_empty_dirs(&base)?;
    }

    if failed > 0 {
        anyhow::bail!("Removed {removed} of {total}. {failed} failed.");
    }

    Ok(())
}

enum BranchDeleteFailure {
    Failed(String),
    Unverified(String),
}

fn delete_branches(
    root: &std::path::Path,
    branches: &[String],
    initial_refs: Option<&HashMap<String, git::MergeTarget>>,
) -> HashMap<String, BranchDeleteFailure> {
    const BATCH_SIZE: usize = 128;

    let fallback_initial_refs = if initial_refs.is_none() {
        git::ref_targets(root).ok()
    } else {
        None
    };
    let initial_refs = initial_refs.or(fallback_initial_refs.as_ref());
    let mut failures = HashMap::new();
    let mut pending = Vec::new();
    for branch in branches {
        let ref_name = format!("refs/heads/{branch}");
        if initial_refs.is_some_and(|refs| !refs.contains_key(&ref_name)) {
            if let Err(error) = git::git_output_in(root, &["branch", "-D", "--", branch]) {
                failures.insert(
                    branch.clone(),
                    BranchDeleteFailure::Failed(format!("{error:#}")),
                );
            }
        } else {
            pending.push(branch.clone());
        }
    }

    for batch in pending.chunks(BATCH_SIZE) {
        let mut args = vec!["branch", "-D", "--"];
        args.extend(batch.iter().map(String::as_str));
        let Err(batch_error) = git::git_output_in(root, &args) else {
            continue;
        };

        match git::ref_oids(root) {
            Ok(current_refs) => {
                for branch in batch {
                    if !current_refs.contains_key(&format!("refs/heads/{branch}")) {
                        if initial_refs.is_none() {
                            failures.insert(
                                branch.clone(),
                                BranchDeleteFailure::Unverified(format!(
                                    "batch deletion failed ({batch_error:#}); could not determine whether the branch existed beforehand"
                                )),
                            );
                        }
                        continue;
                    }
                    if let Err(error) = git::git_output_in(root, &["branch", "-D", "--", branch]) {
                        failures.insert(
                            branch.clone(),
                            BranchDeleteFailure::Failed(format!(
                                "batch deletion failed ({batch_error:#}); retry failed ({error:#})"
                            )),
                        );
                    }
                }
            }
            Err(ref_error) => {
                for branch in batch {
                    failures.insert(
                        branch.clone(),
                        BranchDeleteFailure::Unverified(format!(
                            "batch deletion failed ({batch_error:#}); could not inspect refs ({ref_error:#})"
                        )),
                    );
                }
            }
        }
    }

    failures
}

fn select_worktrees(
    items: &[(String, Option<String>)],
    ann: &WorktreeAnnotations,
    check_refs: &[String],
) -> Result<Vec<(String, Option<String>)>> {
    let term = Term::stderr();
    if !term.is_term() {
        anyhow::bail!(
            "clean needs a terminal to choose worktrees; pass --yes to remove the default selection, or --dry-run to preview it"
        );
    }
    let count = items.len();
    let mut checked: Vec<bool> = items
        .iter()
        .map(|(path, branch)| initially_checked(path, branch.as_deref(), ann))
        .collect();
    let mut selected_count = checked.iter().filter(|value| **value).count();
    let mut cursor = count;
    let mut display = SelectorDisplay::new(&term, items, ann, cursor);

    term.hide_cursor()?;
    let result = (|| -> Result<Vec<(String, Option<String>)>> {
        display.draw(&term, &checked, cursor, selected_count)?;
        loop {
            let previous_cursor = cursor;
            match term.read_key_raw()? {
                Key::ArrowUp | Key::Char('k') if cursor > 0 => cursor -= 1,
                Key::ArrowDown | Key::Char('j') if cursor < count => cursor += 1,
                Key::Char(' ')
                    if cursor < count && !ann.protected.contains_key(&items[cursor].0) =>
                {
                    checked[cursor] = !checked[cursor];
                    if checked[cursor] {
                        selected_count += 1;
                    } else {
                        selected_count -= 1;
                    }
                }
                Key::Char(key @ ('d' | 'l' | 's')) if cursor < count => {
                    let view = match key {
                        'd' => details::View::Diff,
                        'l' => details::View::Log,
                        _ => details::View::Status,
                    };
                    term.clear_last_lines(display.line_count())?;
                    details::show(
                        &term,
                        std::path::Path::new(&items[cursor].0),
                        check_refs,
                        view,
                    )?;
                    display = SelectorDisplay::new(&term, items, ann, cursor);
                    display.draw(&term, &checked, cursor, selected_count)?;
                    continue;
                }
                Key::Enter if cursor == count => {
                    term.clear_last_lines(display.line_count())?;
                    break Ok(items
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| checked[*i])
                        .map(|(_, item)| item.clone())
                        .collect());
                }
                Key::Escape | Key::Char('q') => {
                    term.clear_last_lines(display.line_count())?;
                    break Ok(vec![]);
                }
                Key::CtrlC => {
                    let _ = term.clear_last_lines(display.line_count());
                    let _ = term.show_cursor();
                    eprintln!("cancelled; no selected deletions were applied");
                    std::process::exit(130);
                }
                _ => continue,
            }
            display.update(
                &term,
                items,
                ann,
                SelectorUpdate {
                    checked: &checked,
                    previous_cursor,
                    cursor,
                    selected_count,
                },
            )?;
        }
    })();
    let cursor_result = term.show_cursor();
    if result.is_ok() {
        cursor_result?;
    }
    result
}

fn initially_checked(path: &str, branch: Option<&str>, ann: &WorktreeAnnotations) -> bool {
    !ann.protected.contains_key(path)
        && (!ann.dirty.contains(path) || ann.force)
        && !ann.unchanged.contains(path)
        && (worktree_reason(path, branch, ann) == "merged"
            || ann.unique_commits.get(path).copied() == Some(0))
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
                protected: &HashMap::new(),
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
            protected: &HashMap::new(),
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
