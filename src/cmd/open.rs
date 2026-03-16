use anyhow::Result;

use super::create::{self, CreateOptions};
use crate::{git, worktree};

pub fn run(branch: Option<&str>, agent: bool, args: &[String]) -> Result<()> {
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
    let tool = if agent { "agent" } else { "editor" };
    let (cmd, configured_args) = super::resolve_tool_command_in(&root, tool)?;
    let args: Vec<&str> = configured_args
        .iter()
        .map(|arg| arg.as_str())
        .chain(args.iter().map(|arg| arg.as_str()))
        .collect();
    git::exec_command(&cmd, &args, &dir)
}
