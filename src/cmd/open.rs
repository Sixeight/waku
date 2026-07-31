use anyhow::Result;

use super::create::{self, CreateOptions};
use crate::{git, worktree};

pub fn run(
    branch: Option<&str>,
    agent_command: Option<&str>,
    editor_command: Option<&str>,
    args: &[String],
) -> Result<()> {
    let root = worktree::repo_root()?;
    let waku_config = git::config_get_regexp_in(&root, r"^waku\.")?;
    let dir = match branch {
        None => std::env::current_dir()?,
        Some(b) => match worktree::resolve_worktree_with_config(&root, b, &waku_config) {
            Ok(dir) => dir,
            Err(_) => create::run(
                b,
                CreateOptions {
                    quiet: true,
                    root: Some(root.clone()),
                    ..Default::default()
                },
            )?,
        },
    };
    let (tool, command_override) = if let Some(command) = agent_command {
        ("agent", Some(command))
    } else if let Some(command) = editor_command {
        ("editor", Some(command))
    } else {
        ("editor", None)
    };
    let (cmd, configured_args) = super::resolve_tool_command_with_override(
        &waku_config,
        tool,
        command_override.filter(|command| !command.is_empty()),
    )?;
    let args: Vec<&str> = configured_args
        .iter()
        .map(|arg| arg.as_str())
        .chain(args.iter().map(|arg| arg.as_str()))
        .collect();
    git::exec_command(&cmd, &args, &dir)
}
