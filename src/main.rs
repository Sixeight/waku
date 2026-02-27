use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use git_waku::cmd;

#[derive(Parser)]
#[command(name = "git-waku", about = "Git worktree runner")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Arguments passed through to `git worktree`
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new worktree with symlinks and hooks
    #[command(alias = "create")]
    New {
        /// Branch name to create
        branch: String,

        /// Start Claude Code after creation
        #[arg(short = 'a', long = "ai", conflicts_with = "editor")]
        ai: bool,

        /// Start Neovim after creation
        #[arg(short = 'e', long = "editor", conflicts_with = "ai")]
        editor: bool,

        /// Base ref to create the branch from
        #[arg(long = "from")]
        from: Option<String>,
    },
    /// Open a worktree in Neovim or Claude Code
    Open {
        /// Branch name (uses current directory if omitted)
        branch: Option<String>,

        /// Open with Claude Code instead of Neovim
        #[arg(short = 'a', long = "ai")]
        ai: bool,

        /// Arguments passed through to the launched tool
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Print worktree path for a branch (use with: cd $(git waku path <branch>))
    #[command(alias = "cd")]
    Path {
        /// Branch name
        branch: String,
    },
    /// Remove a specific worktree
    Rm {
        /// Branch name, directory name, or path
        query: String,

        /// Force removal of dirty worktree
        #[arg(short, long)]
        force: bool,

        /// Keep the branch after removing the worktree
        #[arg(long)]
        keep_branch: bool,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Remove merged worktrees
    Clean {
        /// Show what would be removed without removing
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,

        /// Force removal of worktrees with modified or untracked files
        #[arg(short, long)]
        force: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::New {
            branch,
            ai,
            editor,
            from,
        }) => cmd::new::run(&branch, ai, editor, from.as_deref()),
        Some(Command::Path { branch }) => cmd::path::run(&branch),
        Some(Command::Open { branch, ai, args }) => cmd::open::run(branch.as_deref(), ai, &args),
        Some(Command::Rm {
            query,
            force,
            keep_branch,
        }) => cmd::rm::run(&query, force, keep_branch),
        Some(Command::Completions { shell }) => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "git-waku",
                &mut std::io::stdout(),
            );
            Ok(())
        }
        Some(Command::Clean { dry_run, yes, force }) => cmd::clean::run(dry_run, yes, force),
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
