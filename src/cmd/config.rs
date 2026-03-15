use std::env;

use anyhow::{Result, bail};

use crate::git;

pub fn run(global: bool, add: bool, unset: bool, key: &str, value: Option<&str>) -> Result<()> {
    if add && unset {
        bail!("--add and --unset cannot be used together");
    }

    let dir = env::current_dir()?;
    let mut args = vec!["config"];

    if global {
        args.push("--global");
    }

    if unset {
        if value.is_some() {
            bail!("--unset does not accept a value");
        }
        args.push("--unset");
        args.push(key);
        return git::git_in(&dir, &args);
    }

    if add {
        let Some(value) = value else {
            bail!("--add requires a value");
        };
        args.push("--add");
        args.push(key);
        args.push(value);
        return git::git_in(&dir, &args);
    }

    match value {
        Some(value) => {
            args.push(key);
            args.push(value);
            git::git_in(&dir, &args)
        }
        None => {
            args.push("--get");
            args.push(key);
            let value = git::git_output_in(&dir, &args)?;
            println!("{value}");
            Ok(())
        }
    }
}
