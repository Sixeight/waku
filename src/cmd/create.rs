use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use console::style;

use super::{
    collect_worktreeinclude_files, copy_recursive, remove_existing, spinner, WorktreeIncludeMode,
};
use crate::{git, worktree};

pub fn run(branch: &str, ai: bool, editor: bool, from: Option<&str>) -> Result<()> {
    let root = worktree::repo_root()?;
    let wt_path = worktree::worktree_path(&root, branch)?;

    create_worktree(&wt_path, branch, from)?;

    let waku_config = git::config_get_regexp(r"^waku\.")?;

    create_symlinks(&root, &wt_path, &waku_config)?;
    create_copies(&root, &wt_path, &waku_config)?;
    process_worktreeinclude(&root, &wt_path, &waku_config)?;
    run_post_create_hooks(&wt_path, &waku_config)?;

    if ai {
        git::exec_command("claude", &[], &wt_path)?;
    } else if editor {
        git::exec_command("nvim", &[], &wt_path)?;
    } else {
        eprintln!();
        eprintln!(
            "  {} {}",
            style("→").cyan(),
            style(format!("cd $(git waku path {branch})")).dim(),
        );
    }

    Ok(())
}

fn create_worktree(wt_path: &Path, branch: &str, from: Option<&str>) -> Result<()> {
    if wt_path.exists() {
        eprintln!(
            "  {} Worktree {} already exists",
            style("●").yellow(),
            style(branch).bold(),
        );
        return Ok(());
    }

    let sp = spinner(format!("Creating worktree {branch}"));
    let base_ref = from.unwrap_or("HEAD");
    git::git_output(&[
        "worktree",
        "add",
        "-b",
        branch,
        &wt_path.to_string_lossy(),
        base_ref,
    ])?;
    sp.finish_and_clear();
    eprintln!(
        "  {} Created worktree {}",
        style("✔").green().bold(),
        style(branch).bold(),
    );
    Ok(())
}

fn create_symlinks(root: &Path, wt_path: &Path, config: &[(String, String)]) -> Result<()> {
    let includes = config_values(config, "waku.link.include");

    for name in includes {
        let source = root.join(name);
        if !source.exists() {
            eprintln!(
                "  {} Link source not found: {}",
                style("⚠").yellow(),
                style(source.display()).dim(),
            );
            continue;
        }
        link_entry(&source, &wt_path.join(name))?;
        eprintln!("  {} Linked {}", style("✔").green(), name);
    }
    Ok(())
}

fn create_copies(root: &Path, wt_path: &Path, config: &[(String, String)]) -> Result<()> {
    let copies = config_values(config, "waku.copy.include");
    let valid: Vec<&str> = copies
        .iter()
        .filter(|name| {
            let source = root.join(name);
            if source.exists() {
                return true;
            }
            eprintln!(
                "  {} Copy source not found: {}",
                style("⚠").yellow(),
                style(source.display()).dim(),
            );
            false
        })
        .copied()
        .collect();

    if valid.is_empty() {
        return Ok(());
    }

    copy_entries_parallel(root, wt_path, &valid);
    Ok(())
}

fn process_worktreeinclude(
    root: &Path,
    wt_path: &Path,
    config: &[(String, String)],
) -> Result<()> {
    let mode = WorktreeIncludeMode::from_config(config);
    if mode == WorktreeIncludeMode::Ignore {
        return Ok(());
    }

    let files = collect_worktreeinclude_files(root)?;
    if files.is_empty() {
        return Ok(());
    }

    match mode {
        WorktreeIncludeMode::Copy => {
            let names: Vec<&str> = files.iter().map(|p| p.to_str().unwrap_or_default()).collect();
            copy_entries_parallel(root, wt_path, &names);
        }
        WorktreeIncludeMode::Link => {
            for rel in &files {
                let source = root.join(rel);
                let target = wt_path.join(rel);
                link_entry(&source, &target)?;
                eprintln!("  {} Linked {}", style("✔").green(), rel.display());
            }
        }
        WorktreeIncludeMode::Ignore => unreachable!(),
    }
    Ok(())
}

fn run_post_create_hooks(wt_path: &Path, config: &[(String, String)]) -> Result<()> {
    let hooks = config_values(config, "waku.hook.postcreate");

    for hook in hooks {
        eprintln!("  {} Running {}...", style("▸").cyan(), hook);
        let status = Command::new("sh")
            .args(["-c", hook])
            .current_dir(wt_path)
            .status()
            .with_context(|| format!("failed to run hook: {hook}"))?;
        if status.success() {
            eprintln!("  {} Ran {}", style("✔").green(), hook);
        } else {
            eprintln!(
                "  {} Hook failed: {}",
                style("✘").red().bold(),
                style(hook).bold(),
            );
        }
    }
    Ok(())
}

/// Create a symlink, removing any existing entry at the target path.
fn link_entry(source: &Path, target: &Path) -> Result<()> {
    remove_existing(target)?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    unix_fs::symlink(source, target).with_context(|| {
        format!(
            "failed to symlink {} -> {}",
            target.display(),
            source.display()
        )
    })
}

/// Copy multiple entries from `root` to `wt_path` in parallel.
fn copy_entries_parallel(root: &Path, wt_path: &Path, names: &[&str]) {
    let results: Vec<(&str, Result<()>)> = std::thread::scope(|s| {
        let handles: Vec<_> = names
            .iter()
            .map(|name| {
                let source = root.join(name);
                let target = wt_path.join(name);
                s.spawn(move || -> Result<()> {
                    remove_existing(&target)?;
                    copy_recursive(&source, &target)
                })
            })
            .collect();
        names
            .iter()
            .zip(handles)
            .map(|(name, h)| (*name, h.join().unwrap()))
            .collect()
    });
    for (name, result) in results {
        match result {
            Ok(()) => eprintln!("  {} Copied {}", style("✔").green(), name),
            Err(e) => eprintln!(
                "  {} Failed to copy {}: {}",
                style("✘").red().bold(),
                name,
                e
            ),
        }
    }
}

fn config_values<'a>(config: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    config
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}
