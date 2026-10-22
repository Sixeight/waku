# 2026-10-22 の最適化測定

標準の release バイナリは 1,399,664 bytes から 953,104 bytes に縮小した（31.9%削減）。
48 worktree の `clean --dry-run` は中央値で9.4%、通常の作成は11.2%短縮した。
コピーについては、改善を断定できる結果は得られていない。

## 測定条件

macOS 27.0.1 arm64、Git 2.56.0、Rust 1.99.1 で測定した。
基準は `433d842735f1fcf31ad7b36506747215ba18d188` の release ビルド。
最終比較は各バイナリ1回のウォームアップ後に7回測定し、実行順を交互に入れ替えた。
計測中にビルドやテストは実行していない。
外部ネットワークの代わりにローカルの bare remote を使い、Git の個人設定とシステム設定は除外した。
端末サイズは240列、60行に固定した。

準備、削除後の片付け、動作検証は計測に含めない。
全サンプルで表示、チェックアウト状態、コピー先のファイルハッシュ、削除結果を確認した。
ヘルプ中の実行ファイル名だけを正規化して比較した。
[生データ](benchmarks/2026-10-22.json) に各サンプル、サイズ、バイナリの SHA-256 を保存している。

## 最終比較

変化率は負なら短縮、正なら増加を表す。

| 処理 | 変更前 | 変更後 | 変化率 |
| --- | ---: | ---: | ---: |
| ヘルプ | 7.640 ms | 5.180 ms | -32.2% |
| zsh 補完生成 | 3.777 ms | 3.942 ms | +4.4% |
| clean --dry-run（48 worktree） | 345.001 ms | 312.540 ms | -9.4% |
| path（48 worktree） | 11.177 ms | 11.544 ms | +3.3% |
| 既存 worktree の open | 14.258 ms | 14.270 ms | +0.1% |
| 作成 | 65.415 ms | 58.094 ms | -11.2% |
| 削除 | 47.042 ms | 46.403 ms | -1.4% |
| .worktreeinclude のコピー（640ファイル） | 176.217 ms | 188.348 ms | +6.9% |
| ディレクトリのコピー（640ファイル） | 174.354 ms | 189.092 ms | +8.5% |

`path`、`open`、削除、補完生成では改善を確認できていない。
コピーの中央値は約7〜9%悪化した。
サンプルの範囲は `.worktreeinclude` が変更前136.330〜200.002 ms、変更後156.346〜381.431 ms、ディレクトリコピーが変更前162.370〜186.532 ms、変更後122.805〜222.702 ms と重なっている。
ヘルプも測定範囲が重なるため、中央値の差だけで起動性能の改善とは判断しない。

## ビルド設定の選択

標準設定は `lto=true`、`codegen-units=1`、`panic="abort"`、`strip=true`、`opt-level="s"` とした。
通常のエラーは従来どおり返すが、release ビルドで panic が発生するとスタックを巻き戻さずプロセスを終了する。
`opt-level="z"` なら806,528 bytes（42.4%削減）まで縮む。

4,096ファイルの `.worktreeinclude` コピーを各設定9回測定した追加比較は次のとおり。
実行順は3バイナリを順に交代させ、各バイナリのウォームアップで全ファイルのハッシュ、毎回の実行で Git status を検証した。

| ビルド | サイズ | コピーの中央値 |
| --- | ---: | ---: |
| 基準 | 1,399,664 bytes | 818.587 ms |
| s | 953,104 bytes | 777.177 ms |
| z | 806,528 bytes | 850.691 ms |

この比較では `s` の中央値が `z` より約9%短かったため、速度との両立を優先して `s` を標準にした。
4,096ファイルでもサンプルのばらつきは大きく、コピーの改善率は保証できない。
サイズを優先する場合は `CARGO_PROFILE_RELEASE_OPT_LEVEL=z make install` で再現できる。

## 処理の変更

- 作成時の `checkout.workers` を waku 設定と一緒に読み、Git プロセスを1回減らした。
  設定済みの値を上書きせず、値のない設定も保持して Git 自身の検証を通す。
- 表示されない進捗アニメーションでは、スタイルの生成と更新スレッドを省略した。
- `clean` のエージェント検査を dirty 判定と並行させた。
  削除直前のエージェント検査と worktree 状態の再確認は維持した。
- indicatif を0.18.6に更新し、console を0.16.6に統一した。

## 検証と再現

全181テストが成功した（unit 66、fetch 5、integration 104、dirty check 6）。
通常は除外される実プロセス検査も実行した。
`cargo clippy --all-targets -- -D warnings`、変更した Rust モジュールの Rustfmt、`git diff --check` が成功した。
リポジトリ全体の `cargo fmt -- --check` は、変更対象外の `src/main.rs`、`src/cmd/config.rs` と既存の結合テストの整形差分を報告する。

`make install` で標準設定をインストールし、測定バイナリとインストール先の SHA-256 が一致することを確認した。
ネットワーク、実際のエディタ起動、post-create hook の時間は測定対象に含まない。

```sh
python3 scripts/benchmark.py \
  --binary baseline=/tmp/git-waku-baseline \
  --binary optimized=target/release/git-waku \
  --repetitions 7 --output /tmp/waku-benchmark.json

COLUMNS=240 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 \
  cargo test -- --include-ignored
```

`clean` の検証には `ps` と `lsof` で実プロセスを調べられる権限が必要。
規模を増やす場合はベンチマークに `--candidates 192 --files 4096` を指定する。
