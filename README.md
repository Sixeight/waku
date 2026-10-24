# git-waku

**waku** (枠) — a frame, a dedicated space.

`git-waku` creates, opens, and cleans up Git worktrees. It shares dependencies
with new worktrees, launches your editor or AI agent, and removes worktrees
whose branches have been merged.

## Install

Requires a Rust toolchain.

```sh
make install        # installs to ~/.local/bin/git-waku
```

Git then picks it up as `git waku`.

## Usage

### Create

```sh
git waku create my-feature
git waku create my-feature --from origin/main   # branch from a specific ref
git waku create my-feature --agent              # open with Claude Code
git waku create my-feature --editor             # open with Neovim
git waku create my-feature --cd                 # start a shell in the worktree
git waku create my-feature --no-fetch           # use cached refs only
git waku create my-feature --fetch              # wait for the fetch first
```

Worktrees go in a sibling `{repo}-worktrees/` directory. Slashes in branch
names become dashes:

```
myrepo/
myrepo-worktrees/
  my-feature/
  sixeight-some-branch/    # from sixeight/some-branch
```

With `waku.create.fetch` enabled, `create` starts from cached refs, fetches in
the background, and fast-forwards the new branch when it can.

### Path and open

```sh
cd $(git waku path my-feature)              # by branch or directory name
git waku open my-feature                    # launch Neovim
git waku open my-feature --agent -- --resume # pass args through to the agent
git waku open                               # open the current directory
```

### Remove

```sh
git waku remove my-feature                  # remove worktree and branch
git waku remove my-feature --keep-branch
git waku remove my-feature --force          # even if dirty
```

### Clean

```sh
git waku clean                              # interactive selector
git waku clean --dry-run
git waku clean --yes                        # skip confirmation
git waku clean --yes --force                # also remove dirty worktrees
```

Merged and squash-merged worktrees are selected by default when they are clean.
Dirty worktrees, unchanged branches, and branches with unique commits are listed
but left unchecked. Worktrees used by a running AI agent (Claude Code, Codex, OpenCode,
Gemini CLI, aider, Grok CLI) cannot be selected, even with `--force`.

In the selector, press `d` for a diff, `l` for unmerged commits, and `s` for
status. See [docs/clean.md](docs/clean.md) or `git waku clean --help` for the
selection rules and safety checks.

### Other commands

Unrecognized subcommands are forwarded to `git worktree`, so `git waku list`
runs `git worktree list`.

```sh
git waku completions zsh > _git-waku        # also bash, fish
```

## Configuration

All settings use `git config`. See [docs/configuration.md](docs/configuration.md)
for details.

| Key | Description |
| --- | --- |
| `waku.worktrees.path` | Worktree directory, absolute or relative to the repo root |
| `waku.link.include` | Paths symlinked from the main worktree, e.g. `node_modules` |
| `waku.copy.include` | Paths copied from the main worktree, e.g. `.direnv` |
| `waku.copy.exclude` | Paths skipped inside `waku.copy.include` entries |
| `waku.worktreeinclude` | `.worktreeinclude` mode: `copy` (default), `link`, or `ignore` |
| `waku.command.agent` | Agent command (default: `claude`) |
| `waku.command.editor` | Editor command (default: `nvim`) |
| `waku.hook.postCreate` | Shell commands run after `create` |
| `waku.hook.postcreate.background` | Run hooks without waiting; output goes to `waku-post-create.log` in the worktree's Git directory |
| `waku.create.fetch` | Fetch origin when creating |

```sh
git config --add waku.link.include node_modules
git config --add waku.copy.include .direnv
git config waku.command.agent "claude --resume"
git config --add waku.hook.postCreate "direnv allow"
```

`--copy-from-base` keeps copy targets at the new worktree's base revision.
`-a="codex --model gpt-5"` overrides the agent command for one launch.

### `.worktreeinclude`

A `.worktreeinclude` file at the repository root selects gitignored files to
bring into new worktrees. It uses `.gitignore` syntax, including `!` negation.

```
# .worktreeinclude
**/.env
node_modules/
!.claude/worktrees/**
```

## Performance

See [docs/performance.md](docs/performance.md) for benchmarks and how to run
them.

## Acknowledgements

Inspired by [git-worktree-runner](https://github.com/coderabbitai/git-worktree-runner).

## License

MIT
