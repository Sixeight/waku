use anyhow::Result;

use super::create::{self, CreateOptions};
use crate::{git, worktree};

pub fn run(
    branch: Option<&str>,
    agent: Option<String>,
    editor: Option<String>,
    args: &[String],
) -> Result<()> {
    let dir = match super::resolve_dir(branch) {
        Ok(dir) => dir,
        Err(_) if branch.is_some() => create::run(
            branch.unwrap(),
            CreateOptions {
                quiet: true,
                ..Default::default()
            },
        )?,
        Err(e) => return Err(e),
    };
    let root = worktree::repo_root()?;
    let (tool, command_override) = if let Some(command_override) = agent.as_deref() {
        ("agent", Some(command_override))
    } else if let Some(command_override) = editor.as_deref() {
        ("editor", Some(command_override))
    } else {
        ("editor", None)
    };
    let (cmd, configured_args) =
        super::resolve_tool_command_with_override_in(&root, tool, command_override)?;
    let args: Vec<&str> = configured_args
        .iter()
        .map(|arg| arg.as_str())
        .chain(args.iter().map(|arg| arg.as_str()))
        .collect();
    git::exec_command(&cmd, &args, &dir)
}
