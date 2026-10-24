# Optimization measurements on 2026-10-22

The default release binary shrank from 1,399,664 bytes to 953,104 bytes (31.9%
smaller). The median of `clean --dry-run` with 48 worktrees is 9.4% shorter,
and a normal create is 11.2% shorter. No result demonstrates an improvement in
copying.

## Measurement conditions

Measured on macOS 27.0.1 arm64 with Git 2.56.0 and Rust 1.99.1.
The baseline is the release build of `433d842735f1fcf31ad7b36506747215ba18d188`.
The final comparison uses one warmup per binary, followed by 7 measured runs
with the binary order alternated. No builds or tests ran during measurement.
A local bare remote replaces the external network, and Git personal and system
configuration are excluded. The terminal size is fixed at 240 columns by 60
rows.

Fixture setup, cleanup after removal, and behavior checks are outside the timed
interval. Every sample verifies the output, the checkout state, the file hashes
at the copy destination, and the removal result. Only the executable name in
the help output is normalized for comparison.
The [raw data](benchmarks/2026-10-22.json) stores each sample, the sizes, and
the SHA-256 of the binaries.

## Final comparison

A negative change means shorter; a positive change means longer.

| Scenario | Baseline | Optimized | Change |
| --- | ---: | ---: | ---: |
| Help | 7.640 ms | 5.180 ms | -32.2% |
| zsh completion generation | 3.777 ms | 3.942 ms | +4.4% |
| clean --dry-run (48 worktrees) | 345.001 ms | 312.540 ms | -9.4% |
| path (48 worktrees) | 11.177 ms | 11.544 ms | +3.3% |
| open existing worktree | 14.258 ms | 14.270 ms | +0.1% |
| Create | 65.415 ms | 58.094 ms | -11.2% |
| Remove | 47.042 ms | 46.403 ms | -1.4% |
| .worktreeinclude copy (640 files) | 176.217 ms | 188.348 ms | +6.9% |
| Directory copy (640 files) | 174.354 ms | 189.092 ms | +8.5% |

No improvement is demonstrated for `path`, `open`, remove, or completion
generation. The copy medians are about 7–9% worse. The sample ranges overlap:
`.worktreeinclude` is 136.330–200.002 ms before and 156.346–381.431 ms after,
and the directory copy is 162.370–186.532 ms before and 122.805–222.702 ms
after. The help ranges also overlap, so the median difference alone is not
taken as a startup improvement.

## Build settings

The default settings are `lto=true`, `codegen-units=1`, `panic="abort"`,
`strip=true`, and `opt-level="s"`. Ordinary errors are returned as before, but
a panic in a release build terminates the process without unwinding the stack.
`opt-level="z"` shrinks the binary further, to 806,528 bytes (42.4% smaller).

An additional comparison measured a 4,096-file `.worktreeinclude` copy 9 times
per setting. The three binaries took turns in the run order. Each binary's
warmup verified the hashes of all files, and every run verified Git status.

| Build | Size | Copy median |
| --- | ---: | ---: |
| Baseline | 1,399,664 bytes | 818.587 ms |
| s | 953,104 bytes | 777.177 ms |
| z | 806,528 bytes | 850.691 ms |

In this comparison the median of `s` was about 9% shorter than that of `z`, so
`s` is the default to favor both speed and size. Sample variance is large even
with 4,096 files, so no copy improvement is guaranteed. To prioritize size,
reproduce it with `CARGO_PROFILE_RELEASE_OPT_LEVEL=z make install`.

## Code changes

- Create reads `checkout.workers` together with the waku settings, which saves
  one Git process. It does not overwrite a configured value, and it also keeps a
  key set without a value so that Git's own validation still applies.
- Progress animations that are not displayed skip style generation and the
  update thread.
- The agent check in `clean` runs in parallel with the dirty check.
  The agent check just before removal and the recheck of the worktree state are
  kept.
- indicatif is updated to 0.18.6 and console is unified at 0.16.6.

## Verification and reproduction

All 181 tests passed (unit 66, fetch 5, integration 104, dirty check 6).
The real-process checks that are normally excluded also ran.
`cargo clippy --all-targets -- -D warnings`, Rustfmt on the changed Rust
modules, and `git diff --check` passed.
`cargo fmt -- --check` on the whole repository reports formatting differences
in `src/main.rs`, `src/cmd/config.rs`, and existing integration tests, which are
outside this change.

`make install` installed a build with the default settings, and the SHA-256 of
the measured binary matched the installed one.
Network, actual editor startup, and post-create hook time are not measured.

```sh
python3 scripts/benchmark.py \
  --binary baseline=/tmp/git-waku-baseline \
  --binary optimized=target/release/git-waku \
  --repetitions 7 --output /tmp/waku-benchmark.json

COLUMNS=240 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  cargo test -- --include-ignored
```

Verifying `clean` requires permission to inspect real processes with `ps` and
`lsof`.
To increase the scale, pass `--candidates 192 --files 4096` to the benchmark.
