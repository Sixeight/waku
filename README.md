# git-waku

[![CI](https://github.com/Sixeight/waku/actions/workflows/ci.yml/badge.svg)](https://github.com/Sixeight/waku/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Sixeight/waku)](https://github.com/Sixeight/waku/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**waku** (枠) — a frame, a dedicated space.

`git-waku` is a fast Git worktree manager built for parallel branches and AI
coding agents. It creates worktrees, shares ignored dependencies with them,
launches your editor or agent, and safely cleans up merged worktrees.

```sh
git waku create feat/auth --agent   # create worktree, share deps, launch agent
git waku open feat/auth             # reopen in your editor (or --agent)
git waku clean                      # inspect and remove merged worktrees
```

## Fast

Most tools either block on the network to fetch or start from stale refs. With
`waku.create.fetch`, `create` returns from cached refs and fast-forwards the new
branch when the background fetch lands, so you get the fetched commit without
the wait.

Median `create` time against the fastest of worktrunk, gtr, git-wt, gwq, wtp,
rabi, and workz:

| Scenario | waku | Next fastest |
| --- | ---: | ---: |
| 22-file repository | **6.2 ms** | 7.7 ms (wtp) |
| Copy 640 files in `node_modules/` | **10.5 ms** | 17.7 ms (workz) |
| Copy 5,000 files (200 MB) | **45.7 ms** | 76.4 ms (workz) |
| django, about 7,000 files | **97.3 ms** | 219.7 ms (gwq) |
| Origin with 300 ms latency, fresh result | **7.2 ms** | 552.0 ms (gtr) |

Against an origin that answers after 300 ms, waku is even faster than tools
that skip fetching (`wtp` at 8.4 ms, `workz` at 8.7 ms), while `gtr` (552.0 ms)
is the only other tool that ends on the fetched commit. On django, waku finishes
under 100 ms — more than twice as fast as the next tool — because it runs
`git worktree add` with `checkout.workers=8`, and still ends on the fetched
commit.

Measured on Linux (btrfs on NVMe) with Git 2.56.0. See
[docs/performance.md](docs/performance.md) for the full table, method, and how
to reproduce.

## Install

Git discovers an executable named `git-waku` on your `PATH` as `git waku`.

### GitHub Release

Download the archive and matching `.sha256` file from
[GitHub Releases](https://github.com/Sixeight/waku/releases):

| OS | CPU | Archive |
| --- | --- | --- |
| Linux | x86_64 | `git-waku-x86_64-unknown-linux-gnu.tar.gz` |
| Linux | arm64 | `git-waku-aarch64-unknown-linux-gnu.tar.gz` |
| macOS | Apple Silicon | `git-waku-aarch64-apple-darwin.tar.gz` |
| macOS | Intel | `git-waku-x86_64-apple-darwin.tar.gz` |

Verify, extract, and install onto your `PATH` (Linux x86_64 example; on macOS
use `shasum -a 256 -c` instead of `sha256sum -c`):

```sh
sha256sum -c git-waku-x86_64-unknown-linux-gnu.tar.gz.sha256
tar xzf git-waku-x86_64-unknown-linux-gnu.tar.gz
install -m 755 git-waku-x86_64-unknown-linux-gnu/git-waku ~/.local/bin/git-waku
git waku --version
```

### From source

Requires Rust 1.85 or later (`cargo` installs to `~/.cargo/bin`):

```sh
cargo install --git https://github.com/Sixeight/waku --locked
```

## Usage

### Create

```sh
git waku create my-feature
git waku create my-feature --from origin/main     # branch from a specific ref
git waku create my-feature --from-default-branch  # branch from origin's default branch
git waku create my-feature --agent                # open with your AI agent (default: claude)
git waku create my-feature -a="codex --model gpt-5"
git waku create my-feature --editor               # open with your editor (default: nvim)
git waku create my-feature --cd                   # start a shell in the worktree
git waku create my-feature --copy-from-base       # keep copy targets at the base revision
git waku create my-feature --no-fetch             # use cached refs only
git waku create my-feature --fetch                # wait for the fetch first
```

Worktrees are created in a sibling `{repo}-worktrees/` directory with slashes
in branch names converted to dashes:

```text
myrepo/
myrepo-worktrees/
  my-feature/
  sixeight-some-branch/    # from sixeight/some-branch
```

### Open and path

```sh
git waku open my-feature                      # launch editor (creates worktree if missing)
git waku open my-feature --agent -- --resume  # pass args through to the agent
git waku open                                 # open the current directory
cd "$(git waku path my-feature)"              # resolve by branch, dir name, or path
```

### Clean

```sh
git waku clean                # interactive selector
git waku clean --dry-run
git waku clean --yes          # remove default selection without prompting
git waku clean --yes --force  # also remove dirty worktrees
```

```text
Clean candidates ([✔] selected by default):
    DEL  BRANCH        REASON  FILES         COMMITS   UPDATED         SUBJECT
  ▸ [✔]  feat/auth     merged  clean         merged    2 hours ago     Add OAuth login
    [ ]  fix/typo      merged  active agent  merged    10 minutes ago  Fix README typo
    [ ]  wip/refactor  closed  dirty         1 unique  2 days ago      WIP: extract parser
    1/3 · 1 selected · d diff / l log / s status
```

- **Squash-merge aware** — detects both regular merges and squash merges via
  `git merge-tree`.
- **Active agent protection** — worktrees used by running Claude Code, Codex,
  OpenCode, Gemini CLI, aider, or Grok CLI processes cannot be selected, even
  with `--force`.
- **Inspect before deleting** — press `d` for diff, `l` for unmerged commits,
  and `s` for status. State is re-verified right before each removal.

See [docs/clean.md](docs/clean.md) or `git waku clean --help` for the full
selection rules and safety checks.

### Remove

```sh
git waku remove my-feature                # remove worktree and branch
git waku remove my-feature --keep-branch
git waku remove my-feature --force        # even if dirty
```

Files copied or symlinked by `git-waku` are ignored during dirty checks.

### Shell completions and passthrough

```sh
git waku completions zsh > _git-waku   # supports bash, elvish, fish, powershell, zsh
```

Aliases: `new` (`create`), `use` (`open`), `cd` (`path`), `rm` (`remove`).
Unrecognized subcommands are forwarded to `git worktree`, so `git waku list`
runs `git worktree list`.

## Configuration

All settings live in `git config`, and `git waku config` wraps it directly. See
[docs/configuration.md](docs/configuration.md) for details.

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
| `waku.hook.postcreate.background` | Run post-create hooks in the background (logs to `waku-post-create.log` in the worktree Git dir) |
| `waku.create.fetch` | Fetch `origin` in the background when creating |
| `waku.create.from-default-branch` | Create new branches from `origin`'s default branch by default |

```sh
git waku config --add waku.link.include node_modules
git waku config --add waku.copy.include .direnv
git waku config waku.command.agent "claude --resume"
git waku config --add waku.hook.postCreate "direnv allow"
git waku config waku.create.fetch true
```

### `.worktreeinclude`

A `.worktreeinclude` file at the repository root selects gitignored files to
bring into new worktrees using `.gitignore` syntax, including `!` negation:

```gitignore
# .worktreeinclude
**/.env
node_modules/
!.claude/worktrees/**
```

## Acknowledgements

Inspired by [git-worktree-runner](https://github.com/coderabbitai/git-worktree-runner).

## License

[MIT](LICENSE)
