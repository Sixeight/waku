use anyhow::Context;
use clap::Parser;
use git_waku::cmd;
use std::io::Write;

mod cli;

use cli::{Cli, Command, CompletionShell};

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::Create {
            branch,
            agent,
            editor,
            cd,
            from,
            fetch,
            no_fetch,
            from_default_branch,
            copy_from_base,
        }) => cmd::create::run(
            &branch,
            cmd::create::CreateOptions {
                agent: agent.is_some(),
                editor: editor.is_some(),
                agent_command: agent.filter(|command| !command.is_empty()),
                editor_command: editor.filter(|command| !command.is_empty()),
                cd,
                from,
                fetch,
                no_fetch,
                from_default_branch,
                copy_from_base,
                ..Default::default()
            },
        )
        .map(|_| ()),
        Some(Command::Open {
            branch,
            agent,
            editor,
            args,
        }) => cmd::open::run(
            branch.as_deref(),
            agent.as_deref(),
            editor.as_deref(),
            &args,
        ),
        Some(Command::Path { branch }) => cmd::path::run(&branch),
        Some(Command::Remove {
            query,
            force,
            keep_branch,
        }) => cmd::remove::run(&query, force, keep_branch),
        Some(Command::Completions { shell }) => {
            let script: &[u8] = match shell {
                CompletionShell::Bash => include_bytes!(concat!(env!("OUT_DIR"), "/bash")),
                CompletionShell::Elvish => include_bytes!(concat!(env!("OUT_DIR"), "/elvish")),
                CompletionShell::Fish => include_bytes!(concat!(env!("OUT_DIR"), "/fish")),
                CompletionShell::PowerShell => {
                    include_bytes!(concat!(env!("OUT_DIR"), "/powershell"))
                }
                CompletionShell::Zsh => include_bytes!(concat!(env!("OUT_DIR"), "/zsh")),
            };
            std::io::stdout()
                .lock()
                .write_all(script)
                .context("failed to write shell completions")
        }
        Some(Command::Clean {
            dry_run,
            yes,
            force,
        }) => cmd::clean::run(dry_run, yes, force),
        Some(Command::Config {
            global,
            add,
            unset,
            key,
            value,
        }) => cmd::config::run(global, add, unset, &key, value.as_deref()),
        Some(Command::BackgroundFetch {
            root,
            worktree,
            merge_ref,
        }) => cmd::create::background_fetch(&root, &worktree, merge_ref.as_deref()),
        Some(Command::BackgroundHooks { worktree, hooks }) => {
            cmd::create::background_hooks(&worktree, &hooks)
        }
        None => cmd::passthrough(&cli.args),
    };

    if let Err(err) = result {
        use console::style;
        eprintln!("{}: {err}", style("error").red().bold());
        for cause in err.chain().skip(1) {
            eprintln!("  {}: {cause}", style("caused by").yellow());
        }
        std::process::exit(1);
    }
}
