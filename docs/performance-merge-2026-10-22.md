# Additional squash-merge detection measurements on 2026-10-22

`clean --dry-run` now checks squash-merge status in batches of 8.
With 192 worktrees whose upstream is deleted, the median dropped from 928.992 ms
to 757.472 ms (18.5%).
With 192 conflicting worktrees, it dropped from 975.601 ms to 783.745 ms (19.7%).

The baseline is the binary after [batched commit count lookup](performance-clean-2026-10-22.md).
The binary grew from 953,152 bytes to 969,696 bytes, an increase of 16,544 bytes
(1.7%). It is 30.7% smaller than the 1,399,664 bytes before the first
optimization.
The build includes uncommitted changes, so identify the binary by the SHA-256 in
the [raw data](benchmarks/2026-10-22-merge.json).

## Behavior and safety

Previously, `git merge-tree --write-tree` ran once per branch.
Now, for each comparison target, unresolved branches are passed 8 at a time to
`git merge-tree --stdin --no-messages --name-only`, with up to 8 batches running
in parallel.
With multiple comparison targets, only branches not yet judged as merged go to
the next target.
A single branch, or a failed batch, falls back to the per-branch check.

The exit code of a batch does not indicate whether each merge has conflicts.
The success status and tree of each merge are read from the NUL-delimited
output. A branch counts as merged only if it has no conflict and its tree equals
the target's tree.
A binary file conflict is not treated as merged, even if the tree equals the
target's.
A newline in a file name is not confused with a result boundary.
Missing output, an unknown status, or a mismatched result count is an error and
falls back to the per-branch check.
Writing to stdin and reading the output run concurrently, which avoids a stall
from a full pipe.

## Normal history

Median of 7 runs per binary, after one warmup per binary, with alternating
order.
The mixed case has equal numbers of unchanged, squash-merged, and
upstream-deleted branches.
In the upstream-deleted case, each branch has one unique commit.
The conflict case adds a conflicting binary file to main after that divergence
and syncs with origin/main.

| Case, worktrees | Before | Batches of 8 | Reduction |
| --- | ---: | ---: | ---: |
| Mixed, 48 | 247.112 ms | 224.318 ms | 9.2% |
| Upstream deleted, 48 | 272.670 ms | 231.515 ms | 15.1% |
| Conflict, 48 | 307.358 ms | 255.776 ms | 16.8% |
| Mixed, 192 | 809.829 ms | 752.206 ms | 7.1% |
| Upstream deleted, 192 | 928.992 ms | 757.472 ms | 18.5% |
| Conflict, 192 | 975.601 ms | 783.745 ms | 19.7% |

For the 192-worktree upstream-deleted case, the range is 903.380–1,272.292 ms
before and 747.554–900.570 ms after.
The other cases have overlapping ranges, so the median differences are results
from this environment only.
After every run, the candidate output matched exactly, and the registered
worktrees and refs were unchanged.
Fetch uses a local bare repository, so network latency is not included.

### Comparison of batch sizes

| Case | 8 | 32 | 192 |
| --- | ---: | ---: | ---: |
| Mixed, 48 | 224.318 ms | 242.027 ms | 247.487 ms |
| Upstream deleted, 48 | 231.515 ms | 272.339 ms | 294.364 ms |
| Conflict, 48 | 255.776 ms | 264.904 ms | 295.054 ms |
| Mixed, 192 | 752.206 ms | 729.433 ms | 767.151 ms |
| Upstream deleted, 192 | 757.472 ms | 740.389 ms | 838.055 ms |
| Conflict, 192 | 783.745 ms | 789.500 ms | 838.774 ms |

Batches of 32 are faster in some 192-worktree cases, but with 48 worktrees they
reduce parallelism.
Batches of 8 were chosen because they were effective across all of these cases.
Optimality for every repository has not been verified.

### Git process launches

Trace2 start events were counted in a separate run from the timing runs.

| Case | merge-tree, per-branch → batched | Total Git launches |
| --- | ---: | ---: |
| Mixed, 48 | 32 → 4 | 95 → 67 |
| Upstream deleted, 48 | 48 → 6 | 110 → 68 |
| Conflict, 48 | 48 → 6 | 110 → 68 |
| Mixed, 192 | 128 → 16 | 335 → 223 |
| Upstream deleted, 192 | 192 → 24 | 398 → 230 |
| Conflict, 192 | 192 → 24 | 398 → 230 |

For the 192-worktree upstream-deleted case, the median child process CPU time
dropped from 3,161.244 ms to 2,141.047 ms (32.3%).
CPU time is the difference in the sum of user and system time from
`RUSAGE_CHILDREN`. It excludes setup and verification.

## Long main history

After the branches were created, 10,000 commits with the same tree were added to
main, and main was synced with origin/main.
Result for 192 worktrees, with one warmup and 7 measured runs.

| Metric | Before | Batches of 8 | Reduction |
| --- | ---: | ---: | ---: |
| Median elapsed time | 1,698.953 ms | 951.608 ms | 44.0% |
| Median child process CPU time | 7,718.253 ms | 2,791.464 ms | 63.8% |

The elapsed time range was 1,569.158–1,880.067 ms before and
873.482–1,184.105 ms after, with no overlap.
In this case too, the checks of candidate output, registered worktrees, and refs
passed in every run.

## Rerun

```sh
python3 scripts/benchmark-clean.py \
  --binary baseline=/tmp/waku-merge-tuning/baseline \
  --binary batch8=/tmp/waku-merge-tuning/batch8 \
  --binary batch32=/tmp/waku-merge-tuning/batch32 \
  --binary batch192=/tmp/waku-merge-tuning/batch192 \
  --candidates 48 192 --case mixed --case gone --case conflict \
  --repetitions 7 --output /tmp/waku-merge-tuning/comparison.json

python3 scripts/benchmark-clean.py \
  --binary baseline=/tmp/waku-merge-tuning/baseline \
  --binary candidate=/tmp/waku-merge-tuning/batch8 \
  --candidates 192 --case gone --history 10000 \
  --repetitions 7 --output /tmp/waku-merge-tuning/history.json
```

## Verification

- RED: first added a test that clean calls the batched check, and confirmed it
  fails with the per-branch check.
- GREEN: verified the parser, result comparison against real Git, 2,048-item
  pipe input and output, multiple comparison targets, conflicts, and the
  per-branch fallback when batching is rejected.
- `COLUMNS=240 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test -- --include-ignored`: 196 passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- Rustfmt on the changed files and `git diff --check`: passed. The repository has existing formatting differences, so unrelated formatting was not applied.
- A Git that rejects batching was simulated with a shim. It was not verified with an actual old Git binary.
- `make install`: passed. The SHA-256 of the measured candidate, the release build, and the installed binary all match: `1f18857729f04b83cba83486d19c9f21e1f65af644c58a35c2cdc21a967e5636`.
