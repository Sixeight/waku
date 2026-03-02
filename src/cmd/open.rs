use anyhow::Result;

use super::create::{self, CreateOptions};
use crate::{git, worktree};

pub fn run(branch: Option<&str>, ai: bool, args: &[String]) -> Result<()> {
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
    let config = git::config_get_regexp_in(&root, r"^waku\.")?;
    let tool = if ai { "ai" } else { "editor" };
    let cmd = super::resolve_tool(&config, tool);
    let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    git::exec_command(&cmd, &args, &dir)
}
