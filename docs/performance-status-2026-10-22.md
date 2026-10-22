# 2026-10-22 の dirty 判定と補完処理の追加測定

生成ファイルの走査、エージェントのパス比較、補完処理のバイナリサイズを調べた。
比較対象は [squash-merge 判定の一括化](performance-merge-2026-10-22.md) 後の969,696 bytesのバイナリ。
最終候補は936,528 bytesで、33,168 bytes（3.4%）小さくなった。
初回最適化前の1,399,664 bytesからは33.1%の削減となる。

[生データ](benchmarks/2026-10-22-status.json) に、最終候補と段階ごとの測定、バイナリの SHA-256 を記録した。
未コミットの変更を含むため、コミットIDだけではバイナリを特定できない。

## dirty 判定

`git status` にコピーとリンクの対象を除外する pathspec を渡し、対象内のファイル列挙を省く。
対象外の未追跡ファイルは `--untracked-files=normal` でディレクトリ単位にまとめる。
検出に必要なのは変更の有無なので、対象外のディレクトリに変更があれば dirty と判定できる。
Git の起動は worktree ごとに必要で、起動回数は減らしていない。

除外には `:(top,literal,exclude)` を使い、設定値の `*`、`[`、改行などを文字どおりに扱う。
`.`、`..`、空のパス要素などを含む設定値は、既存の文字列照合との意味の差を避けるため従来の取得方式を使う。
次の環境変数がある場合も従来方式を使う。
Git が除外を通常の対象パスに解釈したり、大文字と小文字を同一視したりする場合の見落としを避けるためである。

- `GIT_LITERAL_PATHSPECS`
- `GIT_GLOB_PATHSPECS`
- `GIT_NOGLOB_PATHSPECS`
- `GIT_ICASE_PATHSPECS`

ステージ済みと未ステージの変更、入れ子の生成ファイルと同じディレクトリのユーザーファイル、submodule の変更、Git の失敗時の保護をテストした。
非UTF-8の入力を拒否するパーサーのテストも維持した。
実ファイル名が非UTF-8のケースはLinux用テストを追加したが、このmacOSではファイルシステムが作成を拒否するため実行していない。
除外対象のディレクトリ内は列挙しないため、そこでのファイル名のUTF-8検査も行わない。

変更をdirty判定だけに限定した比較では、2,000個の生成ファイルを持つ24 worktreeの中央値が235.991 msから218.534 msへ7.4%短縮した。
子プロセスCPU時間は598.329 msから418.006 msへ30.1%減った。
ただし経過時間の範囲には重なりがある。
この段階ではサイズは969,696 bytesで変わらなかった。

## エージェントのパス比較

検査時に cwd を正規化して重複を取り除き、各候補との比較で再利用する。
削除直前の検査は省かず、その時点のプロセス状態を取得する。
保存したパスは検査結果の中だけで再利用し、検査をまたいだキャッシュは持たない。
シンボリックリンクが変わっても、取得した cwd の比較先が変わらないことをテストした。

32個のcwdと192候補を4周する単体測定では、中央値が283.474 msから8.104 msへ97.1%短縮した。
初期化を含めず、変更前の正規化を繰り返す式と、実際の `ActiveAgents::uses` を比較した。
順序を交互に変え、ウォームアップ1回の後に7回ずつ測った。
[測定コード](../scripts/benchmark-agent-paths.rs) は本体のモジュールを読み込んで使う。

待機する模擬エージェント32プロセスを実際に起動した `clean --dry-run` の比較では、中央値は842.965 msから868.616 msになった。
測定範囲は変更前822.543〜1,049.394 ms、変更後754.163〜991.289 msと重なり、全体の速度改善は確認できなかった。
子プロセスCPU時間は2,220.113 msから2,158.954 msだった。
パス比較の回数削減は確認できるが、全体への効果とは分けて扱う。
この段階のサイズ増加は16 bytesだった。

## 依存featureと補完生成

CLI定義を `src/cli.rs` にまとめ、`build.rs` で5種類のシェル用補完を生成する。
実行時は生成済みの出力を表示する。
`clap_complete` をビルド依存へ移し、実行時の補完生成器を除いた。
内部コマンドは補完生成の対象に含めない。

変更前のBash補完は、内部コマンド名を生成器が処理できず異常終了していた。
回帰テストで先に再現し、変更後はBash、Elvish、Fish、PowerShell、Zshの出力が成功することを確認した。
BashとZshは構文チェックも通った。
公開コマンド8種類のhelpと、3種類の引数エラーは、実行名を `git-waku` にそろえて変更前と完全一致した。

clapの既定featureはhelp、色、引数エラーの文脈、入力候補の提案に使われているため維持した。
indicatifの `wasmbind` を外す構成もビルドしたが、このネイティブ環境では936,512 bytesのままで削減がなく、採用しなかった。
最終候補の936,528 bytesには、その後加えた環境変数への対処も含む。

## 最終候補の測定

各ケースでバイナリの実行順を変え、ウォームアップ1回の後、cleanは7回、CLI起動は31回ずつ測った。
全clean実行で候補表示の完全一致、登録worktreeと参照の不変を確認した。
fetchはローカルのbare repositoryを使い、測定中にビルドやテストは実行していない。

| ケース | 変更前 | 最終候補 |
| --- | ---: | ---: |
| 通常の48 worktree | 243.588 ms | 246.106 ms |
| 通常の192 worktree | 771.512 ms | 805.531 ms |
| 生成ファイル2,000個ずつの24 worktree | 205.386 ms | 200.493 ms |
| `--help` | 3.703 ms | 3.674 ms |
| `completions zsh` | 3.290 ms | 3.158 ms |
| `completions bash` | 異常終了 | 3.377 ms |

通常ケースは全worktreeに固有コミットがあり、upstreamは削除済みとしている。
192件では中央値が4.4%大きくなったが、範囲は変更前741.117〜799.068 ms、変更後759.322〜1,001.864 msと重なる。
今回の変更で通常ケースの経過時間が改善したとは判断しない。
生成ファイルが多いケースも時間の範囲が重なり、2.4%の中央値の差だけで速度改善とは判断しない。
このケースの子プロセスCPU時間は556.032 msから405.564 msへ27.1%減った。
この段階では生成処理の移動なども含んでおり、dirty判定だけの比較とは別の測定である。

## 検証

- 列挙方法、エージェントのパス保存、Bash補完、pathspec環境変数の回帰テストは、先に失敗を確認してから修正した。
- `COLUMNS=240 GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test -- --include-ignored`: 203件成功。
- `cargo clippy --all-targets -- -D warnings`: 成功。
- 公開helpと引数エラーの比較、5種類の補完出力、BashとZshの構文チェック: 成功。
- 非UTF-8の実ファイル名を扱うLinux専用テストは、このmacOSでは未実行。

## 再実行

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

エージェント検査全体の比較は `benchmark-clean.py` の `--agents 32` を使う。
32個の模擬プロセスは一時ディレクトリで待機し、測定後に終了する。
パス比較単体の測定コードは、releaseビルド後に次のように実行できる。

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

`make install` は成功し、測定候補、releaseビルド、インストール先のSHA-256はすべて `db3b0522a71c76722776bab5da72398b32f15a41ce7799f8e7fd6a5440648a36` で一致した。
