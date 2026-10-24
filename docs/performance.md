# Performance measurements

The benchmark compares release binaries against the same temporary Git repositories.
Git global and system configuration are isolated, and fetch uses a local bare
remote. Fixture setup, cleanup, and correctness checks are outside the timed
interval. Each scenario has one warmup per binary, followed by alternating binary
order across measured rounds. The reported duration is the median.

The scenarios cover:

- `clean --dry-run` with 48 worktrees: 16 unchanged, 16 squash-merged, and 16 with
  a deleted upstream and unique commits.
- `path` and `open` with those 48 worktrees. `open` launches `/usr/bin/true`;
  editor startup time is excluded, and `/bin/pwd` verifies its working directory
  outside the timed interval.
- Creating and removing a worktree with a single tracked file.
- Creating a worktree with 640 ignored files, spread over 16 directories, copied
  either via `.worktreeinclude` glob matching or a single `waku.copy.include`
  directory.

Every sample verifies results: clean output must agree between binaries, dry-run
must preserve all registered worktrees, copied file hashes must match, and removal
must delete both the worktree and its branch. The JSON report includes individual
samples, binary hashes, platform, and Git version.

Network fetch latency, editor startup, and post-create hooks are not represented
by these fixtures. Repository size, filesystem, and other running applications
affect the timings. Run the comparison without concurrent builds or tests.

Measurements from 2026-10-22:

- [Speed and binary size](performance-2026-10-22.md)
- [Copy parallelism and directory layout](performance-copy-2026-10-22.md)
- [Clean unique commit counts](performance-clean-2026-10-22.md)
- [Clean squash-merge detection with merge-tree](performance-merge-2026-10-22.md)
- [Dirty checks, agent path matching, and completion size](performance-status-2026-10-22.md)

## Results on 2026-09-22

Measured on macOS 27.0 arm64 with Git 2.55.0, comparing the release build of
`da39f22` against this change. Each value is the median of nine measured runs.

| Scenario | Baseline | Optimized | Speedup |
| --- | ---: | ---: | ---: |
| Clean, 48 worktrees | 513.887 ms | 221.364 ms | 2.32x |
| Path, 48 worktrees | 22.855 ms | 13.989 ms | 1.63x |
| Open existing worktree | 15.564 ms | 16.866 ms | 0.92x |
| Create | 54.100 ms | 50.576 ms | 1.07x |
| Remove | 59.360 ms | 42.066 ms | 1.41x |
| Create with 640 glob-selected files | 177.153 ms | 134.197 ms | 1.32x |
| Create with a 640-file directory | 216.843 ms | 130.529 ms | 1.66x |

Existing-worktree `open` showed no demonstrated improvement: its median was
1.302 ms slower, with overlapping sample ranges (baseline 15.145–20.812 ms,
optimized 14.654–18.939 ms). All behavior checks passed.

Reproduce with a saved baseline release binary:

```sh
python3 scripts/benchmark.py \
  --binary baseline=/tmp/git-waku-baseline \
  --binary optimized=target/release/git-waku \
  --repetitions 9 --output /tmp/waku-benchmark.json
```

Focused scripts take the same `--binary LABEL=PATH` and `--output` options:

- [`scripts/benchmark-clean.py`](../scripts/benchmark-clean.py): `clean --dry-run` with many candidates, including Git subprocess counts
- [`scripts/benchmark-copy.py`](../scripts/benchmark-copy.py): `create` with large copy sets
- [`scripts/benchmark-cli.py`](../scripts/benchmark-cli.py): `--help` and shell completion output

## Implementation

Clean loads branch IDs, remote configuration, and commit display information in
batches. Identical target commits share a merge check, and target tree IDs are
reused during squash-merge detection. The dirty check uses NUL-delimited Git
status with optional index writes disabled. It still protects tracked and
untracked files, including staged changes and dirty submodules.

The dirty check excludes artifacts with Git pathspecs and reports each
untracked directory outside them as one entry instead of listing its files. When configuration or environment
variables change how paths are interpreted, it falls back to the previous
listing. Agent working directories are canonicalized once per inspection and
reused, and agents are inspected again right before removal. Completions are
generated at build time from the shared CLI definition, and the binary prints
the generated output at run time.

When all comparison targets resolve to one commit, unique commit counts for
branches with a deleted upstream are fetched in one batch. With several targets,
or when the batch fails, counts are fetched per worktree.

Squash-merge detection passes candidates to `git merge-tree --stdin` in batches
of eight, running up to eight batches in parallel. If a batch fails, its
candidates are checked one by one. A result with conflicts is never treated as
merged.

Worktree resolution and removal reuse configuration and worktree listings.
Copy uses up to four workers and clean up to eight, capped at the CPU count.
When the copy source is a chain of single directories, parallelism starts where
the tree first branches. Recursive copies within a worker run sequentially, and
workers never spawn nested workers. Copying symlink targets and copy exclusions
keep their existing behavior.

## Validation

- `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test`: 149 passed
  (50 unit tests and 99 integration tests).
- `cargo clippy --all-targets -- -D warnings`: passed.
- Rustfmt on changed Rust files and `git diff --check`: passed. Repository-wide
  `cargo fmt -- --check` still reports pre-existing formatting in untouched
  `src/cmd/config.rs`, `src/main.rs`, and `tests/integration_test.rs`.
- `make install`: passed; installed binary and measured release binary have
  matching SHA-256 hashes.
