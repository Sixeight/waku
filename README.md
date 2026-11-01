# git-waku

**waku** (枠) — a frame, a dedicated space.

`git-waku` is a fast Git worktree manager. It creates worktrees, shares
dependencies with them, launches your editor or AI agent, and removes
worktrees whose branches have been merged.

## Fast

Most tools either wait on the network to fetch or start from stale refs. With
`waku.create.fetch`, `create` returns from cached refs and fast-forwards the new
branch when the background fetch lands, so you get the fetched commit without
the wait.

Median `create` time in ms, waku against the fastest of worktrunk, gtr,
git-wt, gwq, wtp, rabi, and workz:

| Scenario | waku | Next fastest |
| --- | ---: | ---: |
| 22-file repository | 6.9 | 7.7 (wtp) |
| Copy 640 files in `node_modules/` | 13.8 | 17.3 (workz) |
| Copy 5,000 files (200 MB) | 53.2 | 77.2 (workz) |
| django, about 7,000 files | 106.5 | 228.2 (workz) |
| Origin with 300 ms latency, fresh result | 11.0 | 414.7 (gtr) |

Against an origin that answers after 300 ms, wtp and workz return in about
8 ms, faster than waku. They never fetch, so their worktrees stay on the old
commit; gtr is the only other tool that ends on the fetched commit. On django,
waku also ends on the fetched commit, and checks out about twice as fast as the
next tool because it runs `git worktree add` with `checkout.workers=8`.

Measured on Linux (btrfs on NVMe) with Git 2.56.0. See
[docs/performance.md](docs/performance.md) for the full table, method, and how
to reproduce.

## Install

Git discovers an executable named `git-waku` on your `PATH` as `git waku`. Check the install with `git waku --version`.

### GitHub Release

Download the archive and the matching `.sha256` file for your OS and CPU from [GitHub Releases](https://github.com/Sixeight/waku/releases):

| OS | CPU | Archive |
| --- | --- | --- |
| Linux | x86_64 | `git-waku-x86_64-unknown-linux-gnu.tar.gz` |
| Linux | arm64 | `git-waku-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | Apple Silicon | `git-waku-aarch64-apple-darwin.tar.gz` |
| macOS | Intel | `git-waku-x86_64-apple-darwin.tar.gz` |

Verify the archive, extract it, and put `git-waku` on your `PATH`. Linux example for x86_64:

```sh
sha256sum -c git-waku-x86_64-unknown-linux-gnu.tar.gz.sha256
tar xzf git-waku-x86_64-unknown-linux-gnu.tar.gz
install -m 755 git-waku-x86_64-unknown-linux-gnu/git-waku ~/.local/bin/git-waku
git waku --version
```

On macOS, verify with `shasum -a 256 -c` instead of `sha256sum -c`. `~/.local/bin` must be on your `PATH`.

Each archive has a leading directory named after the target. The binary inside is `git-waku`.

### From source

```sh
cargo install --git https://github.com/Sixeight/waku --locked
```

This needs Rust 1.85 or later. `cargo` puts `git-waku` in `~/.cargo/bin`.

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

## Acknowledgements

Inspired by [git-worktree-runner](https://github.com/coderabbitai/git-worktree-runner).

## License

MIT
