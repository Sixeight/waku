use clap::{CommandFactory, ValueEnum};
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "src/cli.rs"]
mod cli;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/cli.rs");
    let output = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing OUT_DIR")?);
    for shell in clap_complete::Shell::value_variants() {
        let cli = cli::Cli::command();
        let mut command = clap::Command::new("git-waku")
            .version(env!("CARGO_PKG_VERSION"))
            .args(cli.get_arguments().cloned())
            .subcommands(
                cli.get_subcommands()
                    .filter(|command| !command.is_hide_set())
                    .cloned(),
            );
        if let Some(about) = cli.get_about() {
            command = command.about(about.clone());
        }
        let mut file = std::fs::File::create(output.join(shell.to_string()))?;
        clap_complete::generate(*shell, &mut command, "git-waku", &mut file);
    }
    Ok(())
}
