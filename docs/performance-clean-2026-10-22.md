# 2026-10-22 の clean 追加測定

`clean --dry-run` の固有コミット数の取得を一括化した。
upstream が削除された192 worktree のケースで、中央値を1,108.832 msから928.970 msへ16.2%短縮した。
比較先の main に1万コミットを追加したケースでは、2,275.925 msから1,601.443 msへ29.6%短縮した。

比較対象は [コピーの追加最適化](performance-copy-2026-10-22.md) 後のバイナリ。
サイズは953,120 bytesから953,152 bytesで、増加は32 bytesだった。
未コミットの変更を含むため、[生データ](benchmarks/2026-10-22-clean.json) のバイナリ SHA-256 で比較対象を特定する。

## 取得方法

従来は、必要な worktree ごとに `git rev-list --count HEAD --not ...` を実行していた。
比較先が1コミットにまとまり、upstream が削除されたブランチが複数ある場合は、`git for-each-ref` の `ahead-behind` でまとめて取得する。

取得した件数はコミットIDで照合し、最初に読んだ worktree の HEAD と一致するものだけ使う。
一致しないものは個別取得する。
一括問い合わせはエージェント検査と並行して実行する。

次の場合は従来の取得方法を使う。

- main と upstream が別のコミットを指し、比較先が複数ある場合。
- 一括取得の対象ブランチが2つ未満の場合。
- 一括取得したコミットIDに一致しない場合。対応するブランチがない detached HEAD も含む。
- Git が一括取得を拒否した場合や、出力を解析できない場合。

複数の比較先では、それらの履歴の和集合から除外して数える必要がある。
単一の比較先に対する件数を流用しない。
削除直前のエージェント検査、HEAD とブランチの再確認、dirty 判定は変更していない。

## 通常の履歴での比較

各バイナリ1回のウォームアップ後、7回ずつ測定した中央値。
混在ケースは unchanged、squash-merged、upstream 削除済みを同数用意した。
upstream 削除済みのケースは、全 worktree に未統合の固有コミットを1つ持たせた。

| ケース | 変更前 | 変更後 | 短縮率 |
| --- | ---: | ---: | ---: |
| 混在、48 worktree | 272.908 ms | 266.727 ms | 2.3% |
| upstream 削除済み、48 worktree | 269.412 ms | 267.877 ms | 0.6% |
| 混在、192 worktree | 835.262 ms | 797.183 ms | 4.6% |
| upstream 削除済み、192 worktree | 1,108.832 ms | 928.970 ms | 16.2% |

48 worktree と混在ケースは測定範囲が重なるため、小さい中央値の差だけで改善とは判断しない。
upstream 削除済み192 worktree でも範囲は一部重なり、変更前1,032.831〜1,314.373 ms、変更後890.018〜1,085.580 msだった。

Git の起動回数は、計測と別の実行で Trace2 の開始イベントを数えた。

| ケース | 個別カウント | 一括カウント | Git の総起動回数 |
| --- | ---: | ---: | ---: |
| 混在、48 worktree | 16 → 0 | 0 → 1 | 110 → 95 |
| upstream 削除済み、48 worktree | 48 → 0 | 0 → 1 | 157 → 110 |
| 混在、192 worktree | 64 → 0 | 0 → 1 | 398 → 335 |
| upstream 削除済み、192 worktree | 192 → 0 | 0 → 1 | 589 → 398 |

## main の履歴が長い場合

ブランチの作成後に main へ同じ tree を持つコミットを1万個追加し、origin/main も同期した。
古い分岐点と長い比較先の履歴でも、一括取得が遅くならないかを確認した。
192 worktree、各7回の比較結果は次のとおり。

| 指標 | 変更前 | 変更後 | 削減率 |
| --- | ---: | ---: | ---: |
| 経過時間の中央値 | 2,275.925 ms | 1,601.443 ms | 29.6% |
| 子プロセス CPU 時間の中央値 | 13,142.008 ms | 7,617.827 ms | 42.0% |

経過時間の測定範囲は変更前2,178.608〜2,386.669 ms、変更後1,528.300〜1,686.415 msで重ならなかった。
CPU 時間は `RUSAGE_CHILDREN` の user と system の差分で、準備と検証は含まない。
複数の子プロセスを合計するため、経過時間より大きくなる。

## 検証と再現

macOS 27.0.1 arm64、Git 2.56.0、Rust 1.99.1で測定した。
すべて一時リポジトリとローカルの bare remote を使い、外部ネットワークの待ち時間は含めない。
Git の個人設定とシステム設定を除外し、端末サイズを240列、60行に固定した。
実行順を交代させ、計測中にはビルドやテストを行っていない。

毎回、候補一覧の表示が基準版と一致し、登録済み worktree と refs が変わらないことを確認した。
準備、動作検証、Trace2 による起動回数の計測は時間の集計から除外した。

全188テストが成功した（unit 69、clean 4、fetch 5、integration 104、dirty check 6）。
単一比較先、一括問い合わせの拒否、比較先の分岐、detached HEAD のテストを追加した。
実プロセスを検査する既存テストも実行し、Clippy、変更した Rust モジュールの Rustfmt、`git diff --check` が成功した。
`make install` 後、測定した候補版とインストール先の SHA-256 が一致することを確認した。

非対応の Git への切り替えは、一括問い合わせだけを拒否し、他の処理を実際の Git に渡す shim で検証した。
古い Git バージョンそのものは実行していない。

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

エージェント保護の検証には、`ps` と `lsof` で実プロセスを検査できる権限が必要。
