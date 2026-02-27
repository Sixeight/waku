use anyhow::Result;

use crate::git;

pub fn run(branch: Option<&str>, ai: bool, args: &[String]) -> Result<()> {
    let dir = super::resolve_dir(branch)?;
    if ai {
        let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        git::exec_command("claude", &args, &dir)
    } else {
        git::exec_command("nvim", &[], &dir)
    }
}
