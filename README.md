# git-waku

**waku** (枠) — a frame, a dedicated space.

Each worktree is a frame for focused work: one branch, one task, one AI agent. `git-waku` sets up these frames so you can stay in flow — creating worktrees with shared dependencies, launching your editor or AI assistant, and cleaning up when the work is done.

## Install

Requires Rust toolchain.

```sh
make install        # installs to ~/.local/bin/git-waku
```

Once installed, Git picks it up as `git waku`.

## Usage

### Create a worktree

```sh
git waku create my-feature
git waku create my-feature --from origin/main   # branch from a specific ref
git waku create my-feature --agent               # create and open with Claude Code
git waku create my-feature --editor             # create and open with Neovim
```

Worktrees are created in a sibling directory named `{repo}-worktrees/`. Slashes in branch names become dashes in directory names:

```
myrepo/
myrepo-worktrees/
  my-feature/
  sixeight-some-branch/    # from sixeight/some-branch
```

### Get worktree path

```sh
cd $(git waku path my-feature)
cd $(git waku path sixeight/feature)        # by branch name
cd $(git waku path sixeight-feature)        # by directory name
```

### Open a worktree

```sh
git waku open my-feature                    # launch Neovim
git waku open my-feature --agent             # launch Claude Code
git waku open my-feature --agent -- --resume # pass args through to claude
git waku open                               # open current directory
```

### Remove a worktree

```sh
git waku remove my-feature                      # remove worktree and delete branch
git waku remove my-feature --keep-branch        # remove worktree, keep branch
git waku remove my-feature --force              # remove even if dirty
```

### Clean up merged worktrees

```sh
git waku clean                              # interactive selector
git waku clean --dry-run                    # preview what would be removed
git waku clean --yes                        # skip confirmation
git waku clean --yes --force                # also remove dirty worktrees
```

Detects both regular merges and squash merges. Detached worktrees (where the branch has been deleted) are also included. Dirty worktrees appear in the list marked as `(dirty)` and are unchecked by default — you can select them to force-remove, or use `--force` to include them all.

Branches that have not diverged from main (no unique commits) are treated as "not yet started" and skipped. As a side effect, fast-forward merged branches are also skipped — use `git waku remove` to remove them manually.

### Passthrough

Any unrecognized subcommand is forwarded to `git worktree`:

```sh
git waku list                               # = git worktree list
```

### Shell completions

```sh
git waku completions zsh > _git-waku
git waku completions bash
git waku completions fish
```

## Configuration

All configuration is done through `git config`.

### Worktree location (`waku.worktrees.path`)

Override where worktrees are created. Default: `{repo}-worktrees/` in the parent directory.

```sh
git config waku.worktrees.path /tmp/worktrees    # absolute path
git config waku.worktrees.path ../worktrees       # relative to repo root
```

### Symlinks (`waku.link.include`)

Share directories or files from the main worktree into new worktrees via symlinks. Useful for large dependency directories like `node_modules` or `vendor` that you don't want to duplicate.

```sh
git config --add waku.link.include node_modules
git config --add waku.link.include vendor
```

### Copies (`waku.copy.include`)

Share files or directories as copies instead of symlinks. Useful when tools don't handle symlinks correctly. Multiple entries are copied in parallel.

```sh
git config --add waku.copy.include .direnv
git config --add waku.copy.include Cargo.lock
```

### `.worktreeinclude`

Place a `.worktreeinclude` file at the repository root to automatically include gitignored files in new worktrees. Uses the same pattern syntax as `.gitignore`, but only matches files that are actually gitignored.

```
# .worktreeinclude
.env
node_modules/
```

Set the mode with `waku.worktreeinclude`:

```sh
git config waku.worktreeinclude copy        # copy files (default)
git config waku.worktreeinclude link        # create symlinks
git config waku.worktreeinclude ignore      # do nothing
```

### Tool commands (`waku.command.*`)

Override the default commands for `--agent` and `--editor`:

```sh
git config waku.command.agent claude      # default: claude
git config waku.command.editor nvim       # default: nvim
git config waku.command.agent "claude --resume"
git config waku.command.agent "claude --append \"review this branch\""
```

### Post-create hooks (`waku.hook.postCreate`)

Run shell commands after worktree creation:

```sh
git config --add waku.hook.postCreate "cp .env.example .env"
git config --add waku.hook.postCreate "direnv allow"
```

## Acknowledgements

Inspired by [git-worktree-runner](https://github.com/coderabbitai/git-worktree-runner).

## License

MIT
