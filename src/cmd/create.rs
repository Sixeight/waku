use anyhow::{Context, Result};
use console::style;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{
    collect_worktreeinclude_files, config_bool, copy_recursive, copy_recursive_parallel,
    remove_existing, spinner, WorktreeIncludeMode,
};
use crate::{git, worktree};

#[derive(Default)]
pub struct CreateOptions {
    pub agent: bool,
    pub editor: bool,
    pub agent_command: Option<String>,
    pub editor_command: Option<String>,
    pub cd: bool,
    pub fetch: bool,
    pub no_fetch: bool,
    pub from: Option<String>,
    pub from_default_branch: bool,
    pub copy_from_base: bool,
    pub quiet: bool,
    pub root: Option<PathBuf>,
}

const CONFIG_PATTERN: &str = r"^(waku\.|checkout\.workers$)";

pub fn run(branch: &str, opts: CreateOptions) -> Result<PathBuf> {
    if let Some(root) = opts.root.clone() {
        let waku_config = git::config_get_regexp_in(&root, CONFIG_PATTERN)?;
        return run_with_config(branch, opts, &root, &waku_config, None);
    }
    // Config and refs read from the current directory match the main worktree's
    // when run from it, so all three lookups can start at once.
    let cwd = Path::new(".");
    let (located, config, refs) = std::thread::scope(|s| {
        let config = s.spawn(|| git::config_get_regexp_in(cwd, CONFIG_PATTERN));
        let refs = s.spawn(|| git::branch_refs_exist(cwd, branch));
        (
            worktree::repo_root_and_toplevel(),
            config.join().unwrap(),
            refs.join().unwrap(),
        )
    });
    let (root, toplevel) = located?;
    // A linked worktree may have its own config, so read the main worktree's.
    let waku_config = if toplevel == root {
        config?
    } else {
        git::config_get_regexp_in(&root, CONFIG_PATTERN)?
    };
    run_with_config(branch, opts, &root, &waku_config, refs.ok())
}

pub(super) fn run_with_config(
    branch: &str,
    opts: CreateOptions,
    root: &Path,
    waku_config: &[(String, String)],
    mut branch_refs: Option<(bool, bool)>,
) -> Result<PathBuf> {
    let wt_path = worktree::worktree_path_with_config(root, branch, waku_config)?;
    let fetch = !opts.no_fetch && (opts.fetch || config_bool(waku_config, "waku.create.fetch"));
    let from_default_branch = opts.from_default_branch
        || (opts.from.is_none() && config_bool(waku_config, "waku.create.from-default-branch"));
    let background_fetch = background_fetch_plan(root, branch, &opts, from_default_branch, fetch)?;

    if fetch && background_fetch.is_none() {
        let sp = if opts.quiet {
            None
        } else {
            Some(spinner("Fetching origin".to_string()))
        };
        git::fetch_origin(root)?;
        // The fetch may have added the remote branch.
        branch_refs = None;
        if let Some(sp) = sp {
            sp.finish_and_clear();
        }
        if !opts.quiet {
            eprintln!("  {} Fetched origin", style("✔").green());
        }
    }

    let wti_mode = WorktreeIncludeMode::from_config(waku_config);
    let (wt_result, wti_files) = std::thread::scope(|s| {
        let wti_handle = s.spawn(|| {
            if wti_mode == WorktreeIncludeMode::Ignore {
                return Ok(vec![]);
            }
            collect_worktreeinclude_files(root)
        });
        let wt_result = create_worktree(
            root,
            &wt_path,
            branch,
            &opts,
            from_default_branch,
            waku_config,
            branch_refs,
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
    let created_from_ref = wt_result?;

    create_symlinks(root, &wt_path, waku_config, opts.quiet)?;
    if !opts.copy_from_base {
        create_copies(root, &wt_path, waku_config, opts.quiet)?;
    }
    apply_worktreeinclude(root, &wt_path, wti_mode, wti_files?, opts.quiet)?;
    if config_bool(waku_config, "waku.hook.postcreate.background")
        && !super::config_values(waku_config, "waku.hook.postcreate").is_empty()
    {
        let log_path = start_background_hooks(&wt_path, waku_config)?;
        if !opts.quiet {
            eprintln!(
                "  {} Running post-create hooks in background (log: {})",
                style("↻").cyan(),
                log_path.display(),
            );
        }
    } else {
        run_post_create_hooks(&wt_path, waku_config, opts.quiet)?;
    }

    if let Some(plan) = background_fetch {
        let merge_ref = created_from_ref.as_ref().map(|_| plan.merge_ref.as_str());
        let log_path = start_background_fetch(root, &wt_path, merge_ref)?;
        if !opts.quiet {
            eprintln!(
                "  {} Fetching origin in background; fast-forwarding when possible (log: {})",
                style("↻").cyan(),
                log_path.display(),
            );
        }
    }

    if opts.agent {
        let (cmd, args) = super::resolve_tool_command_with_override(
            waku_config,
            "agent",
            opts.agent_command.as_deref(),
        )?;
        let args: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
        git::exec_command(&cmd, &args, &wt_path)?;
    } else if opts.editor {
        let (cmd, args) = super::resolve_tool_command_with_override(
            waku_config,
            "editor",
            opts.editor_command.as_deref(),
        )?;
        let args: Vec<&str> = args.iter().map(|arg| arg.as_str()).collect();
        git::exec_command(&cmd, &args, &wt_path)?;
    } else if opts.cd {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        git::exec_command(&shell, &[], &wt_path)?;
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

struct BackgroundFetchPlan {
    merge_ref: String,
}

fn background_fetch_plan(
    root: &Path,
    branch: &str,
    opts: &CreateOptions,
    from_default_branch: bool,
    fetch: bool,
) -> Result<Option<BackgroundFetchPlan>> {
    if !fetch || opts.fetch {
        return Ok(None);
    }

    let merge_ref = if let Some(from_ref) = opts.from.as_deref() {
        if !is_origin_tracking_ref(from_ref) {
            return Ok(None);
        }
        from_ref.to_string()
    } else if from_default_branch {
        let Ok(default_ref) = git::remote_default_branch_ref(root) else {
            return Ok(None);
        };
        default_ref
    } else {
        format!("origin/{branch}")
    };

    if !ref_exists(root, &merge_ref) {
        return Ok(None);
    }
    Ok(Some(BackgroundFetchPlan { merge_ref }))
}

fn is_origin_tracking_ref(reference: &str) -> bool {
    reference.starts_with("origin/") || reference.starts_with("refs/remotes/origin/")
}

fn ref_exists(root: &Path, reference: &str) -> bool {
    git::git_output_in(root, &["rev-parse", "--verify", reference]).is_ok()
}

fn start_background_fetch(root: &Path, wt_path: &Path, merge_ref: Option<&str>) -> Result<PathBuf> {
    let git_dir = worktree_git_dir(wt_path)?;
    let log_path = git_dir.join("waku-background-fetch.log");
    let stdout = fs::File::create(&log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;

    let executable = std::env::current_exe().context("failed to locate git-waku executable")?;
    let mut command = Command::new(executable);
    command.arg("__background-fetch").arg(root).arg(wt_path);
    if let Some(merge_ref) = merge_ref {
        command.arg(merge_ref);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .context("failed to start background fetch")?;

    Ok(log_path)
}

fn worktree_git_dir(wt_path: &Path) -> Result<PathBuf> {
    let git_dir = PathBuf::from(git::git_output_in(wt_path, &["rev-parse", "--git-dir"])?);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        wt_path.join(git_dir)
    };
    Ok(git_dir)
}

fn start_background_hooks(wt_path: &Path, config: &[(String, String)]) -> Result<PathBuf> {
    let log_path = worktree_git_dir(wt_path)?.join("waku-post-create.log");
    let stdout = fs::File::create(&log_path)
        .with_context(|| format!("failed to create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;

    let executable = std::env::current_exe().context("failed to locate git-waku executable")?;
    let mut command = Command::new(executable);
    command
        .arg("__background-hooks")
        .arg(wt_path)
        .arg("--")
        .args(super::config_values(config, "waku.hook.postcreate"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("failed to start background hooks")?;

    Ok(log_path)
}

pub fn background_hooks(wt_path: &Path, hooks: &[String]) -> Result<()> {
    let hooks: Vec<&str> = hooks.iter().map(String::as_str).collect();
    run_hooks(wt_path, &hooks, false)
}

pub fn background_fetch(root: &Path, wt_path: &Path, merge_ref: Option<&str>) -> Result<()> {
    git::fetch_origin(root).context("background fetch failed")?;

    if let Some(merge_ref) = merge_ref {
        if !ref_exists(root, merge_ref) {
            println!("Fetched origin; {merge_ref} is not available for fast-forward.");
            return Ok(());
        }

        if let Some(upstream) = origin_branch_ref(root, merge_ref) {
            let upstream_arg = format!("--set-upstream-to={upstream}");
            git::git_output_in(wt_path, &["branch", &upstream_arg])
                .with_context(|| format!("failed to set upstream to {upstream}"))?;
        }
        let result = git::git_output_in(wt_path, &["merge", "--ff-only", merge_ref])
            .with_context(|| format!("could not fast-forward from {merge_ref}"))?;
        println!("Fetched origin; fast-forward result: {result}");
    } else {
        println!("Fetched origin; no new worktree branch was updated.");
    }

    Ok(())
}

fn origin_branch_ref(root: &Path, reference: &str) -> Option<String> {
    let full_ref = if let Some(name) = reference.strip_prefix("origin/") {
        format!("refs/remotes/origin/{name}")
    } else if reference.starts_with("refs/remotes/origin/") {
        reference.to_string()
    } else {
        return None;
    };

    git::git_output_in(root, &["check-ref-format", &full_ref])
        .ok()
        .filter(|_| ref_exists(root, &full_ref))
        .map(|_| full_ref)
}

fn create_worktree(
    root: &Path,
    wt_path: &Path,
    branch: &str,
    opts: &CreateOptions,
    from_default_branch: bool,
    config: &[(String, String)],
    branch_refs: Option<(bool, bool)>,
) -> Result<Option<String>> {
    let (from, quiet) = (opts.from.as_deref(), opts.quiet);
    if wt_path.exists() {
        if !quiet {
            eprintln!(
                "  {} Worktree {} already exists",
                style("●").yellow(),
                style(branch).bold(),
            );
        }
        return Ok(None);
    }

    let sp = if quiet {
        None
    } else {
        Some(spinner(format!("Creating worktree {branch}")))
    };
    let wt_path_str = wt_path.to_string_lossy();
    let (local_exists, remote_exists) = match branch_refs {
        Some(refs) => refs,
        None => git::branch_refs_exist(root, branch)?,
    };
    let mut add_args = Vec::new();
    if !config.iter().any(|(key, _)| key == "checkout.workers") {
        add_args.extend_from_slice(&["-c", "checkout.workers=8"]);
    }
    add_args.extend_from_slice(&["worktree", "add"]);
    if local_exists {
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
        add_args.extend_from_slice(&[&wt_path_str, branch]);
        git::git_output_in(root, &add_args)?;
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
        Ok(None)
    } else {
        let base_ref = if let Some(from_ref) = from {
            from_ref.to_string()
        } else if from_default_branch {
            git::remote_default_branch_ref(root)
                .context("could not resolve origin/HEAD; run git fetch origin or pass --from")?
        } else if remote_exists {
            format!("origin/{branch}")
        } else {
            "HEAD".to_string()
        };
        add_args.extend_from_slice(&["-b", branch, &wt_path_str, &base_ref]);
        git::git_output_in(root, &add_args)?;
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
        Ok(Some(base_ref))
    }
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
            let names: Vec<&str> = files
                .iter()
                .map(|p| p.to_str().unwrap_or_default())
                .collect();
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

fn run_post_create_hooks(wt_path: &Path, config: &[(String, String)], quiet: bool) -> Result<()> {
    let hooks = super::config_values(config, "waku.hook.postcreate");
    run_hooks(wt_path, &hooks, quiet)
}

fn run_hooks(wt_path: &Path, hooks: &[&str], quiet: bool) -> Result<()> {
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

    let results = crate::parallel::map_with_limit(names, 4, |name| {
        let source = root.join(name);
        let target = wt_path.join(name);
        let result = remove_existing(&target).and_then(|_| {
            if names.len() == 1 {
                copy_recursive_parallel(&source, &target, excludes)
            } else {
                copy_recursive(&source, &target, excludes)
            }
        });
        (*name, result)
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
