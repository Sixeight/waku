# 2026-10-22 の squash-merge 判定の追加測定

`clean --dry-run` の squash-merge 判定を8件ずつ一括処理する。
192 worktree の upstream 削除済みケースは928.992 msから757.472 msへ18.5%短縮した。
競合ありの192 worktree は975.601 msから783.745 msへ19.7%短縮した。

比較対象は [コミット数取得の一括化](performance-clean-2026-10-22.md) 後のバイナリ。
サイズは953,152 bytesから969,696 bytesへ16,544 bytes（1.7%）増えた。
初回の最適化前の1,399,664 bytesに対しては30.7%小さい。
未コミットの変更を含むため、[生データ](benchmarks/2026-10-22-merge.json) の SHA-256 でバイナリを特定する。

## 処理と安全性

従来はブランチごとに `git merge-tree --write-tree` を実行していた。
比較先ごとに未解決のブランチを8件ずつ `git merge-tree --stdin --no-messages --name-only` に渡し、最大8並列で処理する。
比較先が複数あれば、統合済みと判定できなかったものだけ次の比較先へ渡す。
1件だけの処理と、一括処理が失敗した場合は従来の個別判定を使う。

一括処理の終了コードは、各マージの競合の有無を表さない。
NUL 区切りの結果から各マージの成功状態と tree を読み、競合がなく比較先と同じ tree のものだけを統合済みとする。
バイナリファイルの競合では tree が比較先と同じでも統合済みとはしない。
ファイル名に改行があっても結果の境界と混同しない。
出力の欠落、不明な状態、結果数の不一致はエラーとして個別判定へ戻す。
標準入力への書き込みと出力の読み取りを並行し、パイプが埋まる停止を避ける。

## 通常の履歴

各バイナリ1回のウォームアップ後、順番を入れ替えて7回ずつ測った中央値。
混在ケースは unchanged、squash-merged、upstream 削除済みを同数用意した。
upstream 削除済みケースは各ブランチに固有コミットを1つ用意した。
競合ケースはその分岐後、main に競合するバイナリファイルを追加し、origin/main と同期した。

| ケース、worktree 数 | 変更前 | 8件ずつ | 短縮率 |
| --- | ---: | ---: | ---: |
| 混在、48 | 247.112 ms | 224.318 ms | 9.2% |
| upstream 削除済み、48 | 272.670 ms | 231.515 ms | 15.1% |
| 競合あり、48 | 307.358 ms | 255.776 ms | 16.8% |
| 混在、192 | 809.829 ms | 752.206 ms | 7.1% |
| upstream 削除済み、192 | 928.992 ms | 757.472 ms | 18.5% |
| 競合あり、192 | 975.601 ms | 783.745 ms | 19.7% |

192件の upstream 削除済みケースの測定範囲は変更前903.380〜1,272.292 ms、変更後747.554〜900.570 ms。
他のケースには範囲の重なりがあり、中央値の差はこの環境での測定結果である。
全ケースで候補表示の完全一致、登録 worktree と参照の不変を各実行後に確認した。
fetch はローカルの bare repository を使うため、ネットワークの待ち時間は含まない。

### 一括件数の比較

| ケース | 8件 | 32件 | 192件 |
| --- | ---: | ---: | ---: |
| 混在、48 | 224.318 ms | 242.027 ms | 247.487 ms |
| upstream 削除済み、48 | 231.515 ms | 272.339 ms | 294.364 ms |
| 競合あり、48 | 255.776 ms | 264.904 ms | 295.054 ms |
| 混在、192 | 752.206 ms | 729.433 ms | 767.151 ms |
| upstream 削除済み、192 | 757.472 ms | 740.389 ms | 838.055 ms |
| 競合あり、192 | 783.745 ms | 789.500 ms | 838.774 ms |

32件ずつは192件の一部ケースで速いが、48件では並列性が下がった。
今回のケース全体で効果が出た8件ずつを採用した。すべてのリポジトリで最適となることまでは検証していない。

### Git の起動回数

Trace2 の開始イベントを、時間計測とは別の実行で数えた。

| ケース | merge-tree 個別 → 一括 | Git 総起動回数 |
| --- | ---: | ---: |
| 混在、48 | 32 → 4 | 95 → 67 |
| upstream 削除済み、48 | 48 → 6 | 110 → 68 |
| 競合あり、48 | 48 → 6 | 110 → 68 |
| 混在、192 | 128 → 16 | 335 → 223 |
| upstream 削除済み、192 | 192 → 24 | 398 → 230 |
| 競合あり、192 | 192 → 24 | 398 → 230 |

192件の upstream 削除済みケースの子プロセス CPU 時間中央値は3,161.244 msから2,141.047 msへ32.3%減った。
CPU 時間は `RUSAGE_CHILDREN` の user と system の合計差分で、準備と検証を含まない。

## main の履歴が長い場合

ブランチ作成後に main へ同じ tree を持つコミットを1万個追加し、origin/main と同期した。
192 worktree、ウォームアップ1回と測定7回の結果。

| 指標 | 変更前 | 8件ずつ | 削減率 |
| --- | ---: | ---: | ---: |
| 経過時間の中央値 | 1,698.953 ms | 951.608 ms | 44.0% |
| 子プロセス CPU 時間の中央値 | 7,718.253 ms | 2,791.464 ms | 63.8% |

経過時間の範囲は変更前1,569.158〜1,880.067 ms、変更後873.482〜1,184.105 msで重ならなかった。
このケースでも候補表示、登録 worktree、参照の検証は全実行で通った。

## 再実行

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

## 検証

- RED: clean が一括判定を呼ぶテストを先に追加し、従来の個別判定で失敗することを確認した。
- GREEN: パーサー、実 Git での結果比較、2,048件のパイプ入出力、複数比較先、競合、一括処理拒否時の個別処理を確認した。
- `COLUMNS=240 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test -- --include-ignored`: 196件成功。
- `cargo clippy --all-targets -- -D warnings`: 成功。
- 変更対象の Rustfmt と `git diff --check`: 成功。リポジトリ全体には既存の書式差分があるため、無関係な整形は加えていない。
- Git が一括処理を拒否する場合は shim で検証した。実際の古い Git バイナリでは検証していない。
- `make install`: 成功。測定候補、release ビルド、インストール先の SHA-256 はすべて `1f18857729f04b83cba83486d19c9f21e1f65af644c58a35c2cdc21a967e5636` で一致した。
