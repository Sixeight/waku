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

## Implementation

Clean loads branch IDs, remote configuration, and commit display information in
batches. Identical target commits share a merge check, and target tree IDs are
reused during squash-merge detection. The dirty check uses NUL-delimited Git
status with optional index writes disabled. It still protects tracked and
untracked files, including staged changes and dirty submodules.

Worktree resolution and removal reuse configuration and worktree listings.
Copy and clean workers are capped at eight or the available CPU count, whichever
is smaller. A single directory copy splits its immediate children across those
workers; deeper traversal stays sequential within each worker. Symlink following
and configured copy exclusions retain their existing behavior.

## Validation

- `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test`: 149 passed
  (50 unit tests and 99 integration tests).
- `cargo clippy --all-targets -- -D warnings`: passed.
- Rustfmt on changed Rust files and `git diff --check`: passed. Repository-wide
  `cargo fmt -- --check` still reports pre-existing formatting in untouched
  `src/cmd/config.rs`, `src/main.rs`, and `tests/integration_test.rs`.
- `make install`: passed; installed binary and measured release binary have
  matching SHA-256 hashes.
