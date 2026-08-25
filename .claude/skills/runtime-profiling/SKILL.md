---
name: runtime-profiling
description: >-
  twigpui が走っている間のメモリ (RSS) と CPU を測るときに使う。
  重くなった気がする、ファンが回る、放置したら太る、といった疑いの確かめ方、
  `--perf` の起動と読み方、issue にする閾値、過去の測定との比べ方を扱う。
  「メモリ使用量を測って」「CPU を見て」「リークしてないか確かめて」と言われたときにも引く。
  sandbox の中で `ps` `top` `footprint` が operation not permitted で落ちるときにも引く。
---

# runtime-profiling

走っているアプリの RSS と CPU を、アプリ自身に測らせる。

## なぜ外から `ps` で見ないのか

Claude Code の sandbox は `ps` `top` `footprint` を通さない (プロセス一覧の取得
そのものが `operation not permitted`)。sandbox の外で走るのは `cargo *` だけなので、
`cargo run` で立ったプロセスが自分の数字を読むのが唯一の経路になる。

人間が手元で見るなら Activity Monitor でよい。この手順は **Claude が自律的に
測れる形** を優先している。

## 手順

1. **測る前に閾値の表を読む** (下)。数字を見てから閾値を動かさない。
2. 走らせる。window は普通に開くので、測っている間は触らない。時間が来ると自分で quit する。

```sh
# release が基準。TSV が stdout、要約が stderr
cargo run --release -- --fixture fixtures/timeline.json --perf 60 > ./tmp/perf-release.tsv 2> ./tmp/perf-release.err

# debug は前回の debug との比較にだけ使う (CPU が release の数倍出る)
cargo run -- --fixture fixtures/timeline.json --perf 60 > ./tmp/perf-debug.tsv 2> ./tmp/perf-debug.err
```

3. 要約 (`./tmp/*.err` の末尾 4 行) の `perf conditions:` を読む。
4. 11 秒目以降を集計する (下の awk)。
5. 閾値の表と突き合わせ、`docs/perf-record.md` に 1 行足す。
6. 超えたものがあれば issue を立てる。

`--fixture` としか組めない (live は起動だけで 2 リクエスト課金される。`x-api-budget`)。
worktree で走らせるときは `--target-dir` に本体の `target` を渡すと gpui のビルドを使い回せる。

### 何が測られているか

- `rss_kb`: `ps -o rss`。resident set size。Activity Monitor の "Memory" は
  phys_footprint で、Metal の IOSurface や圧縮メモリを含むので RSS より大きく出る。
  傾向を見るには RSS で足りる。
- `cpu_pct`: 直前の sample からの区間で、累積 CPU 時間 (`ps -o time`、10 ms 刻み)
  の差分を壁時計で割ったもの。全スレッド合計なので 100% を超えうる。
- `ps_pcpu`: `ps -o %cpu`。カーネルの減衰平均。起動直後は当てにならないので参考値。

要約は stderr とログファイルの両方に出る。**`perf conditions:` を必ず読む。**
`screen locked` かつ `draws while occluded: yes` の数字は、画面ロック中も fixture の
window が描き続けている数字で (`Cargo.toml` の `[patch.crates-io]`)、本番の idle とは
比べられない。記録には条件ごと書く。

### 最初の 10 秒は捨てる

fixture でもアバターと添付は `pbs.twimg.com` から落とす。起動直後の数秒は
ダウンロード・デコード・再レイアウトが乗る。さらに **fixture は 5 秒後に保留分の
投稿を届ける** (README の "Show New Posts" の節) ので、6 秒目に山が 1 つ出るのは
仕様。idle の判断は **11 秒目以降** の行でする。

```sh
# 11 秒目以降の cpu_pct の平均と最大、rss の最初と最後
awk -F'\t' 'NR > 1 && $1 >= 10000 && $4 != "" { n++; s += $4; if ($4 > m) m = $4; if (!f) f = $2; l = $2 }
  END { printf "idle cpu avg %.1f%% max %.1f%%  rss %d -> %d kB (%+d)\n", s / n, m, f, l, l - f }' ./tmp/perf-release.tsv
```

## 閾値 — 何を issue にするか

**測る前に決めてある。** 数字を見てから決めると、初回の baseline が言い訳になる。

| 見るもの | 条件 | 超えたら |
| --- | --- | --- |
| idle CPU 平均 (11 秒目以降) | release, screen unlocked | **2% を超えたら issue**。入力が無いのに描き直している。`log_level = "debug"` で何が tick しているか見る |
| idle CPU 区間の最大 (11 秒目以降) | 同上 | 20% を超えたら、その秒に何があったかログと突き合わせる。1 回きりなら記録だけ |
| RSS の増分 (11 秒目 → 最後) | 60 秒の idle | **5,120 kB (5 MiB) を超えたら 300 秒で測り直し**、単調に増え続けるなら issue |
| RSS の最大 | 記録の前回比 | **+20% を超えたら issue**。何を足した PR かは `git log` で追える |

閾値は release の数字に当てる。debug は同じ条件の前回との比較にだけ使う。

issue を立てるときは `backlog-triage` に従う。`area:ui` (描画が回っている) か
`area:cache` (画像・タイムラインの保持) に、`priority:` を 1 つ。
`perf conditions:` の 1 行と、要約の 3 行をそのまま貼る。

## 記録

測定の記録は `docs/perf-record.md`。同じ条件 (build, screen) の前回と見比べるための
表で、**測ったら 1 行足す**。このスキルには数字を書かない — 手順と閾値は変わらず、
数字は測るたびに増える。

## 測れないもの

- **操作中の CPU。** 入力が無いので、スクロールや glide の負荷は乗らない (#225)。
- **スレッドの内訳。** `ps -o time` は全スレッド合計。UI スレッドが止まっているかは分からない。
- **sampler 自身の分。** 1 秒に 1 回の `ps` 起動が親側に乗る分は差し引いていない (#224)。
