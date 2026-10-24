# Additional measurements of dirty checks and completions on 2026-10-22

This run examined the scan of generated files, the agent path comparison, and
the binary size of completion handling.
The baseline is the 969,696-byte binary built after
[batching the squash-merge check](performance-merge-2026-10-22.md).
The final candidate is 936,528 bytes, which is 33,168 bytes (3.4%) smaller.
Compared with 1,399,664 bytes before the first optimization, this is a 33.1%
reduction.

The [raw data](benchmarks/2026-10-22-status.json) records the final candidate,
the per-stage measurements, and the SHA-256 of each binary.
The build includes uncommitted changes, so a commit ID alone does not identify
the binary.

## Dirty check

`git status` receives a pathspec that excludes the copy and link targets, so
files inside the targets are not enumerated.
Untracked files outside the targets are grouped by directory with
`--untracked-files=normal`.
The check only needs to know whether anything changed, so a change in a
directory outside the targets is enough to report dirty.
Git is still started once per worktree. The number of launches is not reduced.

The exclusion uses `:(top,literal,exclude)`, so `*`, `[`, newlines, and similar
characters in setting values are treated literally.
A setting value that contains `.`, `..`, or an empty path component uses the
previous retrieval method, to avoid differences in meaning from the existing
string matching.
The previous method is also used when any of the following environment
variables is set.
This avoids missed files when Git interprets the exclusion as an ordinary
target path or treats upper and lower case as the same.

- `GIT_LITERAL_PATHSPECS`
- `GIT_GLOB_PATHSPECS`
- `GIT_NOGLOB_PATHSPECS`
- `GIT_ICASE_PATHSPECS`

Tests cover staged and unstaged changes, nested generated files next to user
files in the same directory, submodule changes, and the guard for Git failures.
The parser test that rejects non-UTF-8 input is kept.
A Linux-only test for real non-UTF-8 file names was added. It was not run
because the macOS filesystem here refuses to create such names.
Excluded directories are not enumerated, so their file names are not checked
for UTF-8 either.

In the comparison limited to the dirty check, the median for 24 worktrees with
2,000 generated files each fell from 235.991 ms to 218.534 ms, a 7.4% decrease.
Child-process CPU time fell from 598.329 ms to 418.006 ms, a 30.1% decrease.
The elapsed-time ranges overlap, however.
The size stayed at 969,696 bytes at this stage.

## Agent path comparison

During the check, the cwd values are normalized and deduplicated, and reused for
the comparison with each candidate.
The check immediately before removal is not skipped, and it reads the process
state at that moment.
The stored paths are reused only within one check. No cache is kept across
checks.
A test confirms that the comparison target of an already read cwd does not
change when a symbolic link changes.

In a standalone measurement with 32 cwd values and 192 candidates over four
passes, the median fell from 283.474 ms to 8.104 ms, a 97.1% decrease.
Initialization is excluded. The measurement compares an expression that
repeats the previous normalization with the actual `ActiveAgents::uses`.
The order alternated, with one warmup and then seven runs each.
The [measurement code](../scripts/benchmark-agent-paths.rs) loads the main
modules.

In the `clean --dry-run` comparison that started 32 real mock agent processes
that wait, the median went from 842.965 ms to 868.616 ms.
The measured ranges overlap, 822.543 to 1,049.394 ms before and 754.163 to
991.289 ms after, so no overall speed improvement is confirmed.
Child-process CPU time was 2,220.113 ms before and 2,158.954 ms after.
The reduction in path comparisons is confirmed, but it is treated separately
from the effect on the whole.
The size increase at this stage was 16 bytes.

## Dependency features and completion generation

The CLI definition is collected in `src/cli.rs`, and `build.rs` generates
completions for five shells.
At run time, the generated output is printed.
`clap_complete` moved to build dependencies, which removes the completion
generator from the runtime.
Internal commands are not included in completion generation.

Before the change, Bash completion exited abnormally because the generator
could not handle the internal command names.
A regression test reproduced this first. After the change, the output for Bash,
Elvish, Fish, PowerShell, and Zsh succeeds.
Bash and Zsh also passed a syntax check.
The help output of the 8 public commands and 3 kinds of argument errors matched
the pre-change output exactly, with the invoked name set to `git-waku`.

The default features of clap were kept because they provide help, color,
argument error context, and suggestions for input candidates.
A build of indicatif without `wasmbind` was also tried. On this native
environment it stayed at 936,512 bytes with no reduction, so it was not adopted.
The 936,528 bytes of the final candidate include the environment variable
handling added afterward.

## Final candidate measurement

For each case, the execution order of the binaries was varied. After one
warmup, clean ran 7 times and CLI startup ran 31 times each.
Every clean run confirmed an exact match of the displayed candidates and no
change to the registered worktrees and references.
Fetch used a local bare repository, and no builds or tests ran during
measurement.

| Case | Baseline | Final candidate |
| --- | ---: | ---: |
| Normal, 48 worktrees | 243.588 ms | 246.106 ms |
| Normal, 192 worktrees | 771.512 ms | 805.531 ms |
| 24 worktrees with 2,000 generated files each | 205.386 ms | 200.493 ms |
| `--help` | 3.703 ms | 3.674 ms |
| `completions zsh` | 3.290 ms | 3.158 ms |
| `completions bash` | abnormal exit | 3.377 ms |

In the normal cases, every worktree has a unique commit and the upstream is
deleted.
With 192 worktrees, the median is 4.4% larger, but the ranges overlap, 741.117
to 799.068 ms before and 759.322 to 1,001.864 ms after.
This change is not judged to improve elapsed time in the normal cases.
The case with many generated files also has overlapping time ranges, so the
2.4% median difference alone is not judged a speed improvement.
Child-process CPU time in this case fell from 556.032 ms to 405.564 ms, a 27.1%
decrease.
This stage also includes effects such as the move of the generation step, so it
is a different measurement from the comparison limited to the dirty check.

## Verification

- Regression tests for the enumeration method, the agent path storage, Bash
  completion, and the pathspec environment variables were each seen to fail
  before the fix was made.
- `COLUMNS=240 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test -- --include-ignored`: 203 passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- Comparison of public help and argument errors, the output of the 5
  completions, and the Bash and Zsh syntax checks: passed.
- The Linux-only test for real non-UTF-8 file names was not run on this macOS.

## Rerun

```sh
python3 scripts/benchmark-clean.py \
  --binary baseline=/tmp/waku-status-tuning/baseline \
  --binary candidate=/tmp/waku-status-tuning/candidate \
  --candidates 48 192 --case gone --repetitions 7 \
  --output /tmp/waku-status-tuning/combined-final.json

python3 scripts/benchmark-clean.py \
  --binary baseline=/tmp/waku-status-tuning/baseline \
  --binary candidate=/tmp/waku-status-tuning/candidate \
  --candidates 24 --case gone --artifact-files 2000 --repetitions 7 \
  --output /tmp/waku-status-tuning/artifacts-final.json

python3 scripts/benchmark-cli.py \
  --binary baseline=/tmp/waku-status-tuning/baseline \
  --binary candidate=/tmp/waku-status-tuning/candidate \
  --repetitions 31 --output /tmp/waku-status-tuning/cli-final.json
```

The comparison of the whole agent check uses `--agents 32` of
`benchmark-clean.py`.
The 32 mock processes wait in a temporary directory and exit after the
measurement.
The standalone path comparison code can be run as follows after a release
build.

```sh
python3 - <<'PY'
from pathlib import Path
import subprocess
library = next(Path('target/release/deps').glob('libanyhow-*.rlib'))
subprocess.run([
    'rustc', '--edition=2021', '-O', '-Clto', '-Cpanic=abort',
    'scripts/benchmark-agent-paths.rs', '--extern', f'anyhow={library}',
    '-L', 'dependency=target/release/deps',
    '-o', '/tmp/waku-status-tuning/benchmark-agent-paths',
], check=True)
PY
/tmp/waku-status-tuning/benchmark-agent-paths /tmp/waku-status-tuning/path-fixture
```

`make install` succeeded, and the SHA-256 of the measured candidate, the
release build, and the installed binary all match:
`db3b0522a71c76722776bab5da72398b32f15a41ce7799f8e7fd6a5440648a36`.
