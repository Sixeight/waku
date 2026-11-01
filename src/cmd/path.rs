use anyhow::Result;

use crate::{config::Config, worktree};

pub fn run(branch: &str) -> Result<()> {
    let root = worktree::repo_root()?;
    let config = Config::load(&root)?;
    let path = worktree::resolve(&root, branch, &config)?;
    println!("{}", path.display());
    Ok(())
}
