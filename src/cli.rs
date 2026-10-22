use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    #[value(name = "powershell")]
    PowerShell,
    Zsh,
}

#[derive(Parser)]
#[command(name = "git-waku", version, about = "Git worktree runner")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    /// Arguments passed through to `git worktree`
    #[arg(trailing_var_arg = true)]
    pub(crate) args: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Create a new worktree with symlinks and hooks
    #[command(alias = "new")]
    Create {
        /// Branch name to create
        branch: String,

        /// Open with Claude Code after creation
        #[arg(
            short = 'a',
            long = "agent",
            conflicts_with = "editor",
            num_args = 0..=1,
            default_missing_value = "",
            require_equals = true
        )]
        agent: Option<String>,

        /// Open with Neovim after creation
        #[arg(
            short = 'e',
            long = "editor",
            conflicts_with = "agent",
            num_args = 0..=1,
            default_missing_value = "",
            require_equals = true
        )]
        editor: Option<String>,

        /// Start a shell in the worktree after creation
        #[arg(long, conflicts_with_all = ["agent", "editor"])]
        cd: bool,

        /// Base ref to create the branch from
        #[arg(long = "from")]
        from: Option<String>,

        /// Fetch origin before creating the worktree
        #[arg(long = "fetch")]
        fetch: bool,

        /// Skip configured fetch and use the locally cached refs
        #[arg(long = "no-fetch", conflicts_with = "fetch")]
        no_fetch: bool,

        /// Create from origin's default branch
        #[arg(long = "from-default-branch", conflicts_with = "from")]
        from_default_branch: bool,

        /// Keep configured copy targets at their base revision
        #[arg(long = "copy-from-base")]
        copy_from_base: bool,
    },
    /// Open a worktree in Neovim or Claude Code
    #[command(alias = "use")]
    Open {
        /// Branch name (uses current directory if omitted)
        branch: Option<String>,

        /// Open with Claude Code instead of Neovim
        #[arg(
            short = 'a',
            long = "agent",
            conflicts_with = "editor",
            num_args = 0..=1,
            default_missing_value = "",
            require_equals = true
        )]
        agent: Option<String>,

        /// Open with Neovim explicitly
        #[arg(
            short = 'e',
            long = "editor",
            conflicts_with = "agent",
            num_args = 0..=1,
            default_missing_value = "",
            require_equals = true
        )]
        editor: Option<String>,

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
    /// Remove a worktree and its branch
    #[command(alias = "rm")]
    Remove {
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
        shell: CompletionShell,
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
    /// Read and write git-waku configuration
    Config {
        /// Use the global config file
        #[arg(long = "global")]
        global: bool,

        /// Add a new value without replacing existing values
        #[arg(long)]
        add: bool,

        /// Remove the value for a key
        #[arg(long)]
        unset: bool,

        /// Config key
        key: String,

        /// Config value
        value: Option<String>,
    },
    #[command(name = "__background-fetch", hide = true)]
    BackgroundFetch {
        root: PathBuf,
        worktree: PathBuf,
        merge_ref: Option<String>,
    },
    #[command(name = "__background-hooks", hide = true)]
    BackgroundHooks {
        worktree: PathBuf,
        #[arg(last = true)]
        hooks: Vec<String>,
    },
}
