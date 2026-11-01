use anyhow::Result;

use super::create::{self, CreateOptions};
use crate::{config::Config, git, worktree};

pub fn run(
    branch: Option<&str>,
    agent_command: Option<&str>,
    editor_command: Option<&str>,
    args: &[String],
) -> Result<()> {
    let root = worktree::repo_root()?;
    let (tool, command_override) = if let Some(command) = agent_command {
        ("agent", Some(command))
    } else if let Some(command) = editor_command {
        ("editor", Some(command))
    } else {
        ("editor", None)
    };
    let command_override = command_override.filter(|command| !command.is_empty());
    // Config drives worktree resolution and the tool lookup; skip the git
    // spawn when neither needs it (no branch, command fully overridden).
    let waku_config = if branch.is_some() || command_override.is_none() {
        Config::load(&root)?
    } else {
        Config::empty()
    };
    let dir = match branch {
        None => std::env::current_dir()?,
        Some(b) => match worktree::resolve(&root, b, &waku_config) {
            Ok(dir) => dir,
            Err(_) => create::run_with_config(
                b,
                CreateOptions {
                    quiet: true,
                    ..Default::default()
                },
                &root,
                &waku_config,
                None,
            )?,
        },
    };
    let (cmd, configured_args) = super::tool_command(&waku_config, tool, command_override)?;
    let args: Vec<&str> = configured_args
        .iter()
        .map(|arg| arg.as_str())
        .chain(args.iter().map(|arg| arg.as_str()))
        .collect();
    git::exec_command(&cmd, &args, &dir)
}
