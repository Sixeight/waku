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

2026-10-22 の速度とバイナリサイズの比較は [追加の測定記録](performance-2026-10-22.md) を参照。
コピー処理の並列数とディレクトリ構造の比較は [コピーの追加測定](performance-copy-2026-10-22.md) を参照。
clean のコミット数取得の比較は [clean の追加測定](performance-clean-2026-10-22.md) を参照。
clean の squash-merge 判定の比較は [merge-tree の追加測定](performance-merge-2026-10-22.md) を参照。
dirty 判定、エージェントのパス比較、補完処理のサイズは [status と補完の追加測定](performance-status-2026-10-22.md) を参照。

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

dirty 判定は生成物を Git の pathspec で除外し、対象外の未追跡ディレクトリをまとめて取得する。
パスの意味が変わる設定や環境変数がある場合は従来の取得方式を使う。
エージェントの cwd は各検査内で正規化して再利用し、削除直前には再検査する。
補完はビルド時に共通の CLI 定義から生成し、実行時は生成済みの出力を表示する。

比較先が1コミットにまとまる場合、upstream が削除された複数ブランチの固有コミット数も一括取得する。
比較先が複数の場合や一括取得が失敗した場合は、worktree ごとの取得を使う。

squash-merge 判定は8件ずつ `git merge-tree --stdin` に渡し、最大8並列で処理する。
一括処理が失敗した場合は個別判定に戻す。競合した結果を統合済みとは判定しない。

Worktree resolution and removal reuse configuration and worktree listings.
コピーは最大4並列、clean は最大8並列で、CPU数が少ない場合はその数を上限にする。
コピー元が単一ディレクトリの連続なら、枝分かれする場所まで進んでから並列化する。
各ワーカー内の再帰コピーは逐次実行し、ワーカーを入れ子に作らない。
シンボリックリンクの参照先をコピーする動作と、除外設定の動作は維持する。

## Validation

- `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test`: 149 passed
  (50 unit tests and 99 integration tests).
- `cargo clippy --all-targets -- -D warnings`: passed.
- Rustfmt on changed Rust files and `git diff --check`: passed. Repository-wide
  `cargo fmt -- --check` still reports pre-existing formatting in untouched
  `src/cmd/config.rs`, `src/main.rs`, and `tests/integration_test.rs`.
- `make install`: passed; installed binary and measured release binary have
  matching SHA-256 hashes.
