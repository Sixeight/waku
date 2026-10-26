# AGENTS.md

This file provides guidance to coding agents when working with code in this repository.

## Project

git-waku — Git worktree management CLI written in Rust. "waku" (枠) means "frame"; each worktree is a dedicated workspace. Simplifies creating, managing, and cleaning up worktrees with symlink/copy support, editor/AI integration, and squash-merge detection.

## Commands

```bash
# Build & install
cargo build                          # Debug build
make install                         # Release build → ~/.local/bin/git-waku (ALWAYS run after changes)

# Test
cargo test                           # All tests (unit + integration)
cargo test --lib                     # Unit tests only
cargo test --test integration_test   # Integration tests only
cargo test <test_name>               # Single test by name

# Lint
cargo clippy                         # Uses default clippy rules (no config file)

# Run (after install)
git waku create <branch>
git waku clean --dry-run
```

## Architecture

### Entry point & dispatch

`src/cli.rs` defines the clap derive CLI; `src/main.rs` dispatches to `src/cmd/*.rs`, each exposing a `run()` function. `create` spawns the hidden `__background-fetch` / `__background-hooks` subcommands to finish work after it returns. Unrecognized subcommands are forwarded to `git worktree`.

### Core modules

- **`src/git.rs`** (+ `src/git/`) — Git command wrappers (`git_output`, `git_output_in`, `worktree_list`, `is_merge_noop`, `has_branch_diverged`, `exec_command`). All git process invocation goes through here.
- **`src/worktree.rs`** — Path resolution: `repo_root()` (via `--git-common-dir`), `worktrees_base_with_config()`, `resolve_worktree()` (3-strategy lookup: absolute path → branch name → dir name). `resolve_worktree_with_config()` adds a spawn-free fast path for `open`: it verifies the config-derived path by reading the worktree's `.git` linkage file and `HEAD` directly (deliberate perf exception to the git.rs rule).
- **`src/cmd/mod.rs`** — Shared utilities: `spinner()` (toggles "ﾜ"/"ｸ"), file operations (`remove_existing`, `cleanup_empty_dirs`, `copy_recursive`), `.worktreeinclude` processing, git config helpers.

### Key design patterns

- **Parallelism via `std::thread::scope`** — fetch, copy, dirty-check run in parallel (`src/parallel.rs` for per-item work); worktree removal is sequential (git lock).
- **Squash-merge detection** — `is_merge_noop()` uses `git merge-tree --write-tree` to detect squash-merged branches, complementing `git branch --merged`.
- **Artifact-aware dirty checks** — filters out waku-created symlinks/copies before checking worktree dirty state.
- **Config-driven** — all settings via `git config` keys prefixed `waku.` (e.g., `waku.link.include`, `waku.command.agent`).
- **Error handling** — `anyhow::Result` with `with_context()` throughout.

### Integration tests

`tests/integration_test.rs` — creates real temp git repos via `setup_repo()`. Helpers: `run_git()`, `run_waku()` (runs compiled binary), `waku_path()`. Tests cover create/clean/remove/path/open/passthrough commands.
