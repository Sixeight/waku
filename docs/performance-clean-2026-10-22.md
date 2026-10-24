# Additional clean measurements on 2026-10-22

`clean --dry-run` now fetches the unique commit counts in one batch.
With 192 worktrees whose upstream was deleted, the median fell 16.2%, from
1,108.832 ms to 928.970 ms.
With 10,000 commits added to the comparison target main, it fell 29.6%, from
2,275.925 ms to 1,601.443 ms.

The comparison target is the binary after the
[additional copy optimization](performance-copy-2026-10-22.md).
The size went from 953,120 bytes to 953,152 bytes, an increase of 32 bytes.
The build includes uncommitted changes, so the binary SHA-256 in the
[raw data](benchmarks/2026-10-22-clean.json) identifies the comparison target.

## Fetch method

Previously, `git rev-list --count HEAD --not ...` ran for each worktree that
needed it.
When the comparison target is a single commit and several branches have a
deleted upstream, the counts are now fetched together with `ahead-behind` in
`git for-each-ref`.

The fetched counts are matched by commit ID, and only those that match the HEAD
read first for each worktree are used.
Any that do not match are fetched individually.
The batch query runs in parallel with the agent check.

The previous fetch method is used in these cases:

- main and upstream point to different commits, so there are several comparison
  targets.
- Fewer than two branches are eligible for the batch fetch.
- A commit ID does not match the batch result. This includes a detached HEAD
  that has no corresponding branch.
- Git rejects the batch fetch, or the output cannot be parsed.

With several comparison targets, commits must be counted after excluding the
union of their histories.
The count for a single comparison target is not reused.
The agent check just before removal, the re-check of HEAD and branch, and the
dirty check are unchanged.

## Comparison with normal history

Medians of 7 measurements per binary, after one warmup per binary.
The mixed case has equal numbers of unchanged, squash-merged, and
deleted-upstream worktrees.
In the deleted-upstream case, every worktree has one unmerged unique commit.

| Case | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| Mixed, 48 worktrees | 272.908 ms | 266.727 ms | 2.3% |
| Deleted upstream, 48 worktrees | 269.412 ms | 267.877 ms | 0.6% |
| Mixed, 192 worktrees | 835.262 ms | 797.183 ms | 4.6% |
| Deleted upstream, 192 worktrees | 1,108.832 ms | 928.970 ms | 16.2% |

The 48-worktree cases and the mixed case have overlapping measurement ranges,
so the small median differences alone are not judged as improvements.
The ranges also partly overlap for deleted upstream with 192 worktrees: 1,032.831–1,314.373 ms before and 890.018–1,085.580 ms after.

Git launch counts were taken by counting Trace2 start events in a run separate
from the timing.

| Case | Individual counts | Batch counts | Total Git launches |
| --- | ---: | ---: | ---: |
| Mixed, 48 worktrees | 16 → 0 | 0 → 1 | 110 → 95 |
| Deleted upstream, 48 worktrees | 48 → 0 | 0 → 1 | 157 → 110 |
| Mixed, 192 worktrees | 64 → 0 | 0 → 1 | 398 → 335 |
| Deleted upstream, 192 worktrees | 192 → 0 | 0 → 1 | 589 → 398 |

## Long main history

After the branches were created, 10,000 commits with the same tree were added to
main, and origin/main was synced.
This checks that the batch fetch does not slow down with an old branch point and
a long comparison history.
The results for 192 worktrees, 7 runs each, are below.

| Metric | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| Median elapsed time | 2,275.925 ms | 1,601.443 ms | 29.6% |
| Median child process CPU time | 13,142.008 ms | 7,617.827 ms | 42.0% |

The elapsed time ranges did not overlap: 2,178.608–2,386.669 ms before and 1,528.300–1,686.415 ms after.
CPU time is the difference in user and system time from `RUSAGE_CHILDREN`, and
excludes setup and verification.
It sums several child processes, so it exceeds the elapsed time.

## Verification and reproduction

Measured on macOS 27.0.1 arm64, Git 2.56.0, and Rust 1.99.1.
All runs used temporary repositories and a local bare remote, so external
network latency is not included.
Personal and system Git configuration were excluded, and the terminal size was
fixed at 240 columns by 60 rows.
The run order was alternated, and no builds or tests ran during measurement.

Every run confirmed that the candidate list output matched the baseline and
that the registered worktrees and refs were unchanged.
Setup, behavior checks, and launch counting with Trace2 are excluded from the
timing totals.

All 188 tests passed (unit 69, clean 4, fetch 5, integration 104, dirty check
6).
Tests were added for a single comparison target, rejection of the batch query,
divergent comparison targets, and detached HEAD.
Existing tests that inspect real processes also ran, and Clippy, Rustfmt on the
changed Rust modules, and `git diff --check` passed.
After `make install`, the SHA-256 of the measured candidate matched the
installed binary.

The fallback for an unsupported Git was verified with a shim that rejects only
the batch query and passes everything else to the real Git.
An old Git version itself was not run.

```sh
python3 scripts/benchmark-clean.py \
  --binary baseline=/tmp/git-waku-before \
  --binary candidate=target/release/git-waku \
  --candidates 48 192 --repetitions 7 \
  --output /tmp/waku-clean.json

python3 scripts/benchmark-clean.py \
  --binary baseline=/tmp/git-waku-before \
  --binary candidate=target/release/git-waku \
  --candidates 192 --case gone --history 10000 --repetitions 7 \
  --output /tmp/waku-clean-history.json
```

Verifying agent protection requires permission to inspect real processes with
`ps` and `lsof`.
