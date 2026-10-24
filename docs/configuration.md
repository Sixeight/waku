# Configuration

All settings are read from `git config`. `git waku config` is a thin wrapper
around it:

```sh
git waku config waku.command.agent codex          # = git config waku.command.agent codex
git waku config --add waku.link.include vendor
git waku config --unset waku.create.fetch
git waku config --global waku.command.editor hx
```

## Worktree location

`waku.worktrees.path` sets where worktrees are created. Relative paths are
resolved from the repository root. The default is `{repo}-worktrees/` next to
the repository.

## Sharing files with new worktrees

| Key | Effect |
| --- | --- |
| `waku.link.include` | Symlink the path from the main worktree |
| `waku.copy.include` | Copy the path from the main worktree. Entries are copied in parallel |
| `waku.copy.exclude` | Skip a path inside a `waku.copy.include` entry. Relative to the repository root |

All three take multiple values with `git config --add`.

`waku.copy.exclude` does not apply to `.worktreeinclude`.

`create --copy-from-base` leaves `waku.copy.include` paths at the new
worktree's base revision instead of copying them from the main worktree.
Tracked targets stay clean. Untracked targets such as `.direnv` are not copied.

## `.worktreeinclude`

A `.worktreeinclude` file at the repository root selects gitignored files to
bring into new worktrees. Only gitignored, untracked paths are selected.

Git evaluates the patterns with `.gitignore` syntax: `!` negation, root-anchored
paths, directory patterns, and escaping. The last matching pattern wins, subject
to [Git's parent-directory rules](https://git-scm.com/docs/gitignore#_pattern_format).

```
**/.env
node_modules/
!.claude/worktrees/**
```

To exclude `.env` files under `.claude/worktrees/`, negate their contents with
`!.claude/worktrees/**`. Negating the directory alone (`!.claude/worktrees/`)
does not cancel `**/.env`.

A directory is copied or linked as a whole only when everything in it is
selected. Otherwise its selected files are handled one by one.

`waku.worktreeinclude` sets the mode: `copy` (default), `link`, or `ignore`.

## Fetching

| Key | Effect |
| --- | --- |
| `waku.create.fetch` | Fetch origin when creating a worktree |
| `waku.create.from-default-branch` | Create new branches from origin's default branch, like `--from-default-branch` |

With `waku.create.fetch` enabled, `create` starts from cached refs and fetches
in the background. It fast-forwards the new branch from its remote base when it
can, so an agent may see files change while it works. If the branch cannot
fast-forward, it is left alone and the fetch result is written to the worktree's
Git directory.

`--fetch` waits for the fetch before creating. `--no-fetch` skips it, so newly
pushed commits and branches are not available until a later fetch.

## Commands

`waku.command.agent` (default `claude`) and `waku.command.editor` (default
`nvim`) set what `--agent` and `--editor` launch. Arguments are split on
whitespace, with single and double quotes:

```sh
git config waku.command.agent "claude --append \"review this branch\""
```

Override the command for one launch with `=`. Without `=`, the next argument is
the branch name:

```sh
git waku create my-feature -a="codex --model gpt-5"
git waku open -a my-feature
```

## Post-create hooks

`waku.hook.postCreate` holds shell commands to run after `create`. Add one value
per command.

Hooks run synchronously by default. With `waku.hook.postcreate.background` set
to `true`, `create` returns, or launches the agent, without waiting for them.
Their output and failures go to `waku-post-create.log` in the new worktree's
Git directory.
