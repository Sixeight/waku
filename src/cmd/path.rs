use anyhow::Result;

use crate::worktree;

pub fn run(branch: &str) -> Result<()> {
    let path = worktree::resolve_worktree(branch)?;
    println!("{}", path.display());
    Ok(())
}
