use anyhow::Result;

use crate::{git, worktree};

pub fn run(branch: Option<&str>, ai: bool, args: &[String]) -> Result<()> {
    let dir = super::resolve_dir(branch)?;
    let root = worktree::repo_root()?;
    let config = git::config_get_regexp_in(&root, r"^waku\.")?;
    let tool = if ai { "ai" } else { "editor" };
    let cmd = super::resolve_tool(&config, tool);
    let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    git::exec_command(&cmd, &args, &dir)
}
