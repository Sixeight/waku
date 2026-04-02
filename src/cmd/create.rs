use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use anyhow::{Context, Result};
use console::style;

use super::{
    collect_worktreeinclude_files, config_bool, copy_recursive, remove_existing, spinner,
    WorktreeIncludeMode,
};
use crate::{git, worktree};

#[derive(Default)]
pub struct CreateOptions {
    pub agent: bool,
    pub editor: bool,
    pub fetch: bool,
    pub from: Option<String>,
    pub from_default_branch: bool,
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
    let fetch = opts.fetch || config_bool(&waku_config, "waku.create.fetch");
    let from_default_branch = opts.from_default_branch
        || (opts.from.is_none() && config_bool(&waku_config, "waku.create.from-default-branch"));

    if fetch {
        let sp = if opts.quiet {
            None
        } else {
            Some(spinner("Fetching origin".to_string()))
        };
        git::git_output_in(&root, &["fetch", "--prune", "origin"])?;
        if let Some(sp) = sp {
            sp.finish_and_clear();
        }
        if !opts.quiet {
            eprintln!("  {} Fetched origin", style("✔").green());
        }
    }

    // Collect worktreeinclude files in parallel with worktree creation
    // since both only depend on root, not on wt_path.
    let wti_mode = WorktreeIncludeMode::from_config(&waku_config);
    let (wt_result, wti_files) = std::thread::scope(|s| {
        let wti_handle = s.spawn(|| {
            if wti_mode == WorktreeIncludeMode::Ignore {
                return Ok(vec![]);
            }
            collect_worktreeinclude_files(&root)
        });
        let wt_result = create_worktree(
            &root,
            &wt_path,
            branch,
            opts.from.as_deref(),
            from_default_branch,
            opts.quiet,
        );

        let sp = if !opts.quiet && !wti_handle.is_finished() {
            Some(spinner("Collecting files".to_string()))
        } else {
            None
        };
        let wti_files = wti_handle.join().unwrap();
        if let Some(sp) = sp {
            sp.finish_and_clear();
        }

        (wt_result, wti_files)
    });
    wt_result?;

    create_symlinks(&root, &wt_path, &waku_config, opts.quiet)?;
    create_copies(&root, &wt_path, &waku_config, opts.quiet)?;
    apply_worktreeinclude(&root, &wt_path, wti_mode, wti_files?, opts.quiet)?;
    run_post_create_hooks(&wt_path, &waku_config, opts.quiet)?;

    if opts.agent {
        let (cmd, args) = super::resolve_tool_command_in(&root, "agent")?;
        let args: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
        git::exec_command(&cmd, &args, &wt_path)?;
    } else if opts.editor {
        let (cmd, args) = super::resolve_tool_command_in(&root, "editor")?;
        let args: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
        git::exec_command(&cmd, &args, &wt_path)?;
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
    from_default_branch: bool,
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
    let wt_path_str = wt_path.to_string_lossy();
    if git::branch_exists(root, branch) {
        if from.is_some() && !quiet {
            eprintln!(
                "  {} Branch {} already exists, --from is ignored",
                style("⚠").yellow(),
                style(branch).bold(),
            );
        }
        if from_default_branch && !quiet {
            eprintln!(
                "  {} Branch {} already exists, --from-default-branch is ignored",
                style("⚠").yellow(),
                style(branch).bold(),
            );
        }
        git::git_output_in(root, &["worktree", "add", &wt_path_str, branch])?;
    } else {
        let base_ref = if let Some(from_ref) = from {
            from_ref.to_string()
        } else if from_default_branch {
            git::remote_default_branch_ref(root)
                .context("could not resolve origin/HEAD; run git fetch origin or pass --from")?
        } else if git::remote_branch_exists(root, branch) {
            format!("origin/{branch}")
        } else {
            "HEAD".to_string()
        };
        git::git_output_in(root, &[
            "worktree",
            "add",
            "-b",
            branch,
            &wt_path_str,
            &base_ref,
        ])?;
    }
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

    let excludes: Vec<PathBuf> = super::config_values(config, "waku.copy.exclude")
        .into_iter()
        .map(|v| root.join(v.trim_end_matches('/')))
        .collect();

    copy_entries_parallel(root, wt_path, &valid, &excludes, quiet);
    Ok(())
}

fn apply_worktreeinclude(
    root: &Path,
    wt_path: &Path,
    mode: WorktreeIncludeMode,
    files: Vec<PathBuf>,
    quiet: bool,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    match mode {
        WorktreeIncludeMode::Copy => {
            let names: Vec<&str> = files.iter().map(|p| p.to_str().unwrap_or_default()).collect();
            // waku.copy.exclude applies only to waku.copy.include, not .worktreeinclude
            copy_entries_parallel(root, wt_path, &names, &[], quiet);
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
        let mut cmd = Command::new("sh");
        cmd.args(["-c", hook]).current_dir(wt_path);
        if quiet {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        let status = cmd
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
fn copy_entries_parallel(
    root: &Path,
    wt_path: &Path,
    names: &[&str],
    excludes: &[PathBuf],
    quiet: bool,
) {
    let sp = if quiet {
        None
    } else {
        let label = if names.len() <= 3 {
            names.join(", ")
        } else {
            format!("{} (+{} more)", names[..2].join(", "), names.len() - 2)
        };
        Some(spinner(format!("Copying {label}")))
    };

    let results: Vec<(&str, Result<()>)> = std::thread::scope(|s| {
        let handles: Vec<_> = names
            .iter()
            .map(|name| {
                let source = root.join(name);
                let target = wt_path.join(name);
                s.spawn(move || {
                    let result = remove_existing(&target)
                        .and_then(|_| copy_recursive(&source, &target, excludes));
                    (*name, result)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    if let Some(sp) = sp {
        sp.finish_and_clear();
    }
    if !quiet {
        for (name, result) in &results {
            match result {
                Ok(()) => {
                    eprintln!("  {} Copied {}", style("✔").green(), name);
                }
                Err(e) => {
                    eprintln!(
                        "  {} Failed to copy {}: {}",
                        style("✘").red().bold(),
                        name,
                        e
                    );
                }
            }
        }
    }
}
