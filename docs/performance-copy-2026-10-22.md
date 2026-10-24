# Additional copy measurements on 2026-10-22

When a single-directory chain leads to the branching point, worktree creation
with copying is 39-55% shorter. The copy limit is lowered from 8 to 4 parallel
workers, and copying is parallelized only after reaching a directory that
branches. The Git limit stays at 8 parallel workers.

The baseline is the 953,104-byte binary installed for the
[first optimization](performance-2026-10-22.md). After this change the binary is
953,120 bytes, an increase of 16 bytes. The Git HEAD does not include the first
optimization, so the baseline is identified by the SHA-256 in the
[raw data](benchmarks/2026-10-22-copy.json).

## Copy time breakdown

A temporary instrumented build split the time. For a 4,096-file
`.worktreeinclude` copy, the median was 572.946 ms for the whole creation,
522.922 ms for the copy, and 35.003 ms for file enumeration. Enumeration and
worktree creation run concurrently, so the parts do not sum to the total. The
instrumentation is not in the installed binary.

In this all-files-match pattern, the existing code collapsed the files into the
single `cache` directory and copied it as a whole. This was measured separately from the path that
repeats parent directory creation for each file.

## Parallelism comparison

Files of 256 bytes were spread over 16 directories. Comparison builds with 1, 2,
4, and 8 parallel workers were each measured 7 times. Each had 1 warmup, and the
run order was alternated.

| Case | 1 worker | 2 workers | 4 workers | 8 workers |
| --- | ---: | ---: | ---: | ---: |
| include, 640 files | 218.775 ms | 163.566 ms | 127.475 ms | 131.234 ms |
| directory, 640 files | 219.829 ms | 161.708 ms | 133.320 ms | 131.022 ms |
| include, 4,096 files | 1,204.913 ms | 692.628 ms | 490.706 ms | 547.312 ms |
| directory, 4,096 files | 1,158.767 ms | 688.114 ms | 479.520 ms | 569.951 ms |

With 640 files, the difference between 4 and 8 workers was small. With 4,096
files, the 4-worker median was shorter, so copying uses 4 workers. This choice
was made on the local macOS filesystem that was measured. It does not guarantee
the best value on other disks or environments.

## Single-directory chain

Before the change, when the copy source had only one directory directly under
it, that directory was copied sequentially. After the change, the single
directory chain is followed first, and the work is split across at most 4
workers at the branching point. Each worker runs a recursive copy sequentially
and does not create additional workers.

The implemented candidate was measured 7 times.

| Case | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| 640 files | 241.746 ms | 146.825 ms | 39.3% |
| 4,096 files | 1,171.508 ms | 527.285 ms | 55.0% |

For 4,096 files, the measured range did not overlap: 1,134.687-1,331.934 ms
before and 457.731-574.128 ms after. Tests that preserve contents, empty
directories, and exclusion rules were also added.

## Other cases

For the partial-match pattern, each directory held a tracked `.keep` and an
`excluded.bin` that does not match the include pattern. Only `.dat` files were
copied, and every run also verified that non-matching files were not copied.

| Case | Before | After |
| --- | ---: | ---: |
| partial match, 640 files | 141.415 ms | 140.077 ms |
| partial match, 4,096 files | 582.815 ms | 562.817 ms |
| 1 MiB × 64 files | 77.112 ms | 69.156 ms |

The measured ranges overlap, so the median difference alone is not treated as an
improvement.

The regular commands were also run 7 times each with the final binary. Creating
640 files spread over 16 directories went from 174.385 to 174.876 ms with
include and from 164.821 to 173.039 ms with directory. The latter median is
about 5% worse, but the measured ranges overlap: 124.842-174.312 ms before and
133.196-224.023 ms after. `clean`, `path`, `open`, and regular create and remove
are also stored in the raw data. No improvement outside copying was confirmed.

## Method and verification

Measured on macOS 27.0.1 arm64 with Git 2.56.0 and Rust 1.99.1. Git user and
system configuration were excluded, and every run used real temporary
repositories and worktrees. No builds or tests ran during measurement. Setup,
cleanup, and correctness checks were excluded from the timed interval.

The copy script verifies file names, content hashes, the branch, and Git status
on every run. Hashes are collected in bulk with
`git hash-object --stdin-paths --no-filters`. To avoid mistaking missing files
for a speedup, both the candidate and the baseline were checked to produce the
same contents.

All 183 tests passed (unit 68, fetch 5, integration 104, dirty check 6). Tests
that inspect real processes were also run, and Clippy, Rustfmt on the changed
Rust modules, and `git diff --check` passed. After `make install`, the SHA-256
of the installed binary matched the measured candidate.

```sh
python3 scripts/benchmark-copy.py \
  --binary baseline=/tmp/git-waku-before \
  --binary candidate=target/release/git-waku \
  --files 640 4096 --groups 1 --mode directory \
  --repetitions 7 --output /tmp/waku-copy-chain.json

python3 scripts/benchmark-copy.py \
  --binary baseline=/tmp/git-waku-before \
  --binary candidate=target/release/git-waku \
  --files 640 4096 --selective --mode include \
  --repetitions 7 --output /tmp/waku-copy-selected.json
```

## Existing bug found during measurement

With the baseline, I confirmed a case where target files are not copied when an
untracked directory mixes `.worktreeinclude` targets and non-targets. It
reproduces with `.gitignore` containing `cache/`, `.worktreeinclude` containing
`cache/**/*.dat`, and the files `cache/group-00/file-0000.dat` and
`cache/group-00/excluded.bin`. `git ls-files` with
`--directory --no-empty-directory` returned an empty result, and the collection code returned
that empty target list as is.

This bug is not changed by this optimization. The partial-match measurements
used a tracked `.keep`, a condition under which copying already works. These
results do not prove that partial-match copying inside an untracked directory
works correctly.
