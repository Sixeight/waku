use anyhow::Result;

use crate::git;

pub fn run(branch: Option<&str>, ai: bool, args: &[String]) -> Result<()> {
    let dir = super::resolve_dir(branch)?;
    let config = git::config_get_regexp(r"^waku\.command\.")?;
    if ai {
        let cmd = super::resolve_tool(&config, "ai");
        let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        git::exec_command(&cmd, &args, &dir)
    } else {
        let cmd = super::resolve_tool(&config, "editor");
        git::exec_command(&cmd, &[], &dir)
    }
}
