# Cleaning up worktrees

`git waku clean` lists worktrees that are candidates for removal and removes the
ones you select.

## Candidate table

| Column | Values |
| --- | --- |
| `REASON` | `merged`, `closed` (upstream deleted), `detached`, or `unchanged` |
| `FILES` | `clean`, `dirty`, `active agent`, or `agent check unavailable` |
| `COMMITS` | `merged`, or the number of commits unique to the worktree |
| `UPDATED`, `SUBJECT` | The latest commit. Hidden on narrow terminals |

## Default selection

`[✔]` marks the default selection, which `--yes` removes without prompting:

- Clean worktrees whose branch is merged or squash-merged
- Clean `closed` or `detached` worktrees with no unique commits

These stay unchecked:

- Dirty worktrees, unless `--force` is passed
- `unchanged` branches, which have no commits since they were created.
  Fast-forward merged branches can land here
- Worktrees with unique commits, or whose unique commit count is unknown

Select unchecked worktrees by hand, or use `git waku remove`.

## Agent protection

Worktrees used by running Claude Code, Codex, OpenCode, Gemini CLI, aider, or
Grok CLI processes, including sessions waiting for input, are marked
`active agent`. They cannot be selected, even with `--force`.

The check looks at the working directories of local processes. Remote sessions
and desktop apps whose processes run elsewhere may not be detected. If the
check fails, every worktree is kept and marked `agent check unavailable`.

## Checks before removal

Before removing each worktree, `clean`:

1. Checks for active agents again.
2. Verifies that the branch and HEAD have not changed since listing.
3. Without `--force`, checks that a worktree listed as clean is still clean.

Dirty worktrees selected by hand are removed without the third check.

## Selector keys

| Key | Action |
| --- | --- |
| `↑` / `↓`, `k` / `j` | Move |
| `Space` | Toggle selection |
| `d` | Diff against the primary worktree's branch, including uncommitted tracked changes |
| `l` | Commits absent from that branch and its upstream |
| `s` | Status, including untracked files |
| `Enter` | Remove the selection (on the confirm row) |
| `Esc`, `q` | Quit without removing |

In the `d`, `l`, and `s` views, scroll with the arrow keys, `j` / `k`, or
Page Up/Down. `Enter`, `Esc`, or `q` returns to the list.
