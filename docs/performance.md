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

Focused scripts take the same `--binary LABEL=PATH` and `--output` options:

- [`scripts/benchmark-clean.py`](../scripts/benchmark-clean.py): `clean --dry-run` with many candidates, including Git subprocess counts
- [`scripts/benchmark-copy.py`](../scripts/benchmark-copy.py): `create` with large copy sets
- [`scripts/benchmark-cli.py`](../scripts/benchmark-cli.py): `--help` and shell completion output

[`scripts/benchmark-agent-paths.rs`](../scripts/benchmark-agent-paths.rs) times
agent path matching in isolation. Build it against the release dependencies:

```sh
cargo build --release
rustc --edition=2021 -O -Clto -Cpanic=abort scripts/benchmark-agent-paths.rs \
  --extern anyhow="$(ls target/release/deps/libanyhow-*.rlib | head -1)" \
  -L dependency=target/release/deps -o /tmp/benchmark-agent-paths
/tmp/benchmark-agent-paths /tmp/agent-path-fixture
```

## Comparison with other worktree tools

With `waku.create.fetch`, `git waku create` returns before the fetch finishes
and still ends up on the fetched commit. On a 7,000-file repository it checks
out about twice as fast as the other tools measured. On small fixtures, waku is
tied with the fastest tools, and with copied files it was 1.1–1.5x faster than
the next tool.

### Results on 2026-10-25

Measured on Linux 7.2.8 (btrfs on NVMe, 24 cores) with Git 2.56.0. Medians in
ms. 15 samples per scenario; 7 for `heavy` and 10 for `real`. The JSON report
has every sample and the min–max.

| Tool | plain | copy | link | heavy | fetch | real |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| waku | 8.5 | 15.9 | 8.7 | 52.4 | 12.9, fresh after fetch | 108.4, fresh after fetch |
| waku `--fetch` | | | | | 318.3, fresh | 694.8, fresh |
| worktrunk 0.81.0 | 18.2 | 47.5 | | 81.6 | 19.7, stale | 234.4, stale |
| gtr 2.11.1 | 104.6 | 117.7 | | 171.4 | 414.8, fresh | 907.2, fresh |
| git-wt 0.30.0 | 35.0 | 48.0 | 36.3 | 130.6 | 35.9, stale | 281.0, stale |
| gwq 0.1.2 | 8.1 | 22.7 | | 105.7 | | 224.9, stale |
| wtp 2.10.3 | 7.7 | 19.8 | 8.2 | 99.4 | 8.4, stale | 223.2, stale |
| rabi 7.0.0 | 52.0 | 64.8 | | 145.4 | 53.8, stale | 1050.8, stale |
| workz 0.11.0 | 8.2 | 17.3 | 8.4 | 78.0 | 8.9, stale | 223.2, stale |

- `plain`: a 22-file repository, no file sharing.
- `copy`: 640 small gitignored files in `node_modules/`, copied.
- `link`: the same `node_modules/`, symlinked. Blank cells are tools without
  symlink support.
- `heavy`: about 5,000 files and 200 MB in `node_modules/`, copied. btrfs clones
  file data, so this mostly measures file count.
- `fetch`: the origin answers after 300 ms and has one commit the clone lacks.
- `real`: [django](https://github.com/django/django) (about 7,000 tracked
  files). The clone holds objects only up to 20 commits before the tip, so a
  fetch downloads them from GitHub.

"Fresh" means the new worktree contains the remote commit. waku with
`waku.create.fetch` returns with the cached base and fast-forwards when the
background fetch lands; this happened within the settle time in every sample.
gtr and `waku --fetch` wait for the fetch. The other tools do not fetch on
create. In `real`, waku is faster because it runs `git worktree add` with
`checkout.workers=8`.

Each sample uses a fresh HOME and fixture, runs one untimed warmup, and times
one create until the tool process exits. Every sample verifies that the
worktree is registered, fully checked out, on the expected commit, and that
`node_modules/` has the expected type and file count. Tool order rotates each
round.

Notes:

- worktrunk copies through a `pre-start` hook, which blocks until the copy is
  done. Its documented `post-start` hook returns earlier while copying
  continues in the background.
- gwq builds worktree paths from the origin URL, so the fixture's local origin
  URL is replaced. It is excluded from `fetch`.
- waku, gtr, and git-wt run as Git subcommands, which adds about 1 ms.
- macOS (APFS) has not been measured yet.

Reproduce with the tools on `PATH`; missing tools are skipped:

```sh
cargo install worktrunk workz
cargo install --git https://github.com/yoiailabs/rabi
go install github.com/k1LoW/git-wt@latest
go install github.com/d-kuro/gwq/cmd/gwq@latest
go install github.com/satococoa/wtp/v2/cmd/wtp@latest
git clone https://github.com/coderabbitai/git-worktree-runner  # add its bin/ to PATH

cargo build --release
python3 scripts/benchmark-worktree-tools.py --work-dir ~/.cache/waku-tools-benchmark \
  --output /tmp/waku-tools.json
```

Use `--work-dir` on the filesystem you want to measure; the system temporary
directory may be tmpfs. `--scenarios` and `--tools` select a subset.

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
of eight, running up to eight batches in parallel. Batches of 32 were faster
for some 192-worktree fixtures but reduced parallelism with 48 worktrees, so
eight is the size that helped in every measured case. If a batch fails, its
candidates are checked one by one. A result with conflicts is never treated as
merged.

Worktree resolution and removal reuse configuration and worktree listings.
Copy uses up to four workers and clean up to eight, capped at the CPU count.
With 4,096 files on macOS, four copy workers had a shorter median than eight.
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
