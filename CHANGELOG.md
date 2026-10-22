# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-10-19

First public release of `git-waku`.

### Added

- `create`, `open`, `path`, `remove`, `clean`, `config`, and `completions` commands
- Passthrough of unrecognized subcommands to `git worktree`
- Worktree creation with symlink includes, copy includes, and `.worktreeinclude`
- Optional origin fetch, including background fetch when launching an agent
- Post-create hooks, including a background mode
- Interactive `clean` with squash-merge detection and protection for worktrees used by running AI agents
- Editor and agent launch via `waku.command.*` git config

[0.1.0]: https://github.com/Sixeight/waku/releases/tag/v0.1.0
