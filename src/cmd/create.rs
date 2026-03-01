use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use console::style;

use super::{
    collect_worktreeinclude_files, copy_recursive, remove_existing, spinner, WorktreeIncludeMode,
};
use crate::{git, worktree};

#[derive(Default)]
pub struct CreateOptions {
    pub ai: bool,
    pub editor: bool,
    pub from: Option<String>,
    pub quiet: bool,
    pub root: Option<PathBuf>,
}

pub fn run(branch: &str, opts: CreateOptions) -> Result<PathBuf> {
    let root = match opts.root {
        Some(ref r) => r.clone(),
        None => worktree::repo_root()?,
    };
    let waku_config = git::config_get_regexp_in(&root, r"^waku\.")?;
    let wt_path = worktree::worktree_path_with_config(&root, branch, &waku_config)?;

    create_worktree(&root, &wt_path, branch, opts.from.as_deref(), opts.quiet)?;

    create_symlinks(&root, &wt_path, &waku_config, opts.quiet)?;
    create_copies(&root, &wt_path, &waku_config, opts.quiet)?;
    process_worktreeinclude(&root, &wt_path, &waku_config, opts.quiet)?;
    run_post_create_hooks(&wt_path, &waku_config, opts.quiet)?;

    if opts.ai {
        let cmd = super::resolve_tool(&waku_config, "ai");
        git::exec_command(&cmd, &[], &wt_path)?;
    } else if opts.editor {
        let cmd = super::resolve_tool(&waku_config, "editor");
        git::exec_command(&cmd, &[], &wt_path)?;
    } else if !opts.quiet {
        eprintln!();
        eprintln!(
            "  {} {}",
            style("→").cyan(),
            style(format!("cd $(git waku path {branch})")).dim(),
        );
    }

    Ok(wt_path)
}

fn create_worktree(
    root: &Path,
    wt_path: &Path,
    branch: &str,
    from: Option<&str>,
    quiet: bool,
) -> Result<()> {
    if wt_path.exists() {
        if !quiet {
            eprintln!(
                "  {} Worktree {} already exists",
                style("●").yellow(),
                style(branch).bold(),
            );
        }
        return Ok(());
    }

    let sp = if quiet {
        None
    } else {
        Some(spinner(format!("Creating worktree {branch}")))
    };
    let base_ref = from.unwrap_or("HEAD");
    git::git_output_in(root, &[
        "worktree",
        "add",
        "-b",
        branch,
        &wt_path.to_string_lossy(),
        base_ref,
    ])?;
    if let Some(sp) = sp {
        sp.finish_and_clear();
    }
    if !quiet {
        eprintln!(
            "  {} Created worktree {}",
            style("✔").green().bold(),
            style(branch).bold(),
        );
    }
    Ok(())
}

fn create_symlinks(
    root: &Path,
    wt_path: &Path,
    config: &[(String, String)],
    quiet: bool,
) -> Result<()> {
    let includes = super::config_values(config, "waku.link.include");

    for name in includes {
        let source = root.join(name);
        if !source.exists() {
            if !quiet {
                eprintln!(
                    "  {} Link source not found: {}",
                    style("⚠").yellow(),
                    style(source.display()).dim(),
                );
            }
            continue;
        }
        link_entry(&source, &wt_path.join(name))?;
        if !quiet {
            eprintln!("  {} Linked {}", style("✔").green(), name);
        }
    }
    Ok(())
}

fn create_copies(
    root: &Path,
    wt_path: &Path,
    config: &[(String, String)],
    quiet: bool,
) -> Result<()> {
    let copies = super::config_values(config, "waku.copy.include");
    let valid: Vec<&str> = copies
        .iter()
        .filter(|name| {
            let source = root.join(name);
            if source.exists() {
                return true;
            }
            if !quiet {
                eprintln!(
                    "  {} Copy source not found: {}",
                    style("⚠").yellow(),
                    style(source.display()).dim(),
                );
            }
            false
        })
        .copied()
        .collect();

    if valid.is_empty() {
        return Ok(());
    }

    copy_entries_parallel(root, wt_path, &valid, quiet);
    Ok(())
}

fn process_worktreeinclude(
    root: &Path,
    wt_path: &Path,
    config: &[(String, String)],
    quiet: bool,
) -> Result<()> {
    let mode = WorktreeIncludeMode::from_config(config);
    if mode == WorktreeIncludeMode::Ignore {
        return Ok(());
    }

    let sp = if quiet {
        None
    } else {
        Some(spinner("Collecting .worktreeinclude files".to_string()))
    };
    let files = collect_worktreeinclude_files(root);
    if let Some(sp) = sp {
        sp.finish_and_clear();
    }
    let files = files?;
    if files.is_empty() {
        return Ok(());
    }

    match mode {
        WorktreeIncludeMode::Copy => {
            let names: Vec<&str> = files.iter().map(|p| p.to_str().unwrap_or_default()).collect();
            copy_entries_parallel(root, wt_path, &names, quiet);
        }
        WorktreeIncludeMode::Link => {
            for rel in &files {
                let source = root.join(rel);
                let target = wt_path.join(rel);
                link_entry(&source, &target)?;
                if !quiet {
                    eprintln!("  {} Linked {}", style("✔").green(), rel.display());
                }
            }
        }
        WorktreeIncludeMode::Ignore => unreachable!(),
    }
    Ok(())
}

fn run_post_create_hooks(
    wt_path: &Path,
    config: &[(String, String)],
    quiet: bool,
) -> Result<()> {
    let hooks = super::config_values(config, "waku.hook.postcreate");

    for hook in hooks {
        if !quiet {
            eprintln!("  {} Running {}...", style("▸").cyan(), hook);
        }
        let status = Command::new("sh")
            .args(["-c", hook])
            .current_dir(wt_path)
            .status()
            .with_context(|| format!("failed to run hook: {hook}"))?;
        if !quiet {
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
fn copy_entries_parallel(root: &Path, wt_path: &Path, names: &[&str], quiet: bool) {
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
    if !quiet {
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
}

