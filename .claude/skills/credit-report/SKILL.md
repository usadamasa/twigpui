---
name: credit-report
description: >-
  X API のクレジット (プリペイド残高) がいつ・何に消えたかを実績から確かめるときに使う。
  残高を使い切った、思ったより早く尽きた、購入した額と消費の見積りが合わない、
  ログに HTTP 402 credits depleted が並んでいる、といったときが対象。
  「クレジットの消費を可視化して」「今日いくら使ったか見せて」「何時に残高が切れたか調べて」
  と言われたときにも引く。
  これから足す呼び出しの課金を見積もるのは x-api-budget が担当。
---

# credit-report

`scripts/credit-report.sh` が `usage.json` とログを集計し、外部依存の無い HTML を 1 枚書く。
累計消費額の線、5 分ごとの課金 Posts の棒、endpoint ごとの内訳と購入額との差額が載る。

## 最初にデータを退避する

**課金済み post の id (`usage.json` の `dedup.posts.ids`) は UTC 0 時 (09:00 JST) に消える。**
時系列の出どころはこれしか無いので、調べはじめる前にコピーを取る。
`usage.json` はレスポンスのたびに書き換わるため、アプリが動いている間も待たない。

```sh
mkdir -p ./tmp/credit-YYYY-MM-DD
cp ~/.local/state/twigpui/usage.json ~/.local/state/twigpui/logs/twigpui.log ./tmp/credit-YYYY-MM-DD/
```

debug ビルドの消費を見るなら `twigpui-dev` の側 (`app-logs`)。

## ファイルに無い入力はユーザーに聞く

購入した時刻と金額はどのファイルにも無い。ユーザーに聞くか、Developer Console の
明細を見てもらう。無くても描けるが、購入額との差額と残高切れまでの時間が出ない。

## 生成して見せる

```sh
scripts/credit-report.sh --state-dir "$PWD/tmp/credit-YYYY-MM-DD" --purchase 'YYYY-MM-DD HH:MM' --amount 5
```

`--purchase` はローカル時刻。`--state-dir` を省くと本番の state を直接読む。
出力は `./tmp/credit-report.html` (`--out` で変える)。`open` は sandbox の中で失敗するので、
`Artifact` で publish してリンクを渡す。

描けるのは `usage.json` が持っている UTC 日の 1 日分だけ。

## 読み方

- **時刻は課金時刻ではなく post の作成時刻。** 成功した取得の時刻が残っていない日は、
  post id の snowflake から作成時刻を起こして近似している。自動更新が数分おきに先頭ページを
  読むので、ずれはポーリング 1 回ぶんに収まる。購入より前に作られた post は購入時刻へ寄せる。
- **残高切れの印は、最後の課金より後にある最初の `HTTP 402`。** それより前の 402 は
  購入前の残高切れで、印にはしない。ユーザーの記憶している時刻と食い違ったらログの側を信じる。
- **write の件数は失敗したリクエストを含む。** `usage.json` は HTTP の結果を見ずに 1 件と数える。
  スクリプトはログの `<endpoint>: HTTP 4xx` の行数を引いて内訳に出す。
- **差額は単価の載っていない write の分。** いいね・リポスト・List メンバーの削除は
  `x-api-budget` の `reference/pricing.md` に単価が無い (追加は 2026-09-21 に $0.010 / request と実測済み)。
  差額を件数で割った値は目安で、裏取りは Developer Console でしかできない。
- **`Posts の件数が合わない` で止まったら集計を疑う。** posts 種別の endpoint の `today` の和は
  dedup の id 数と一致する (`usage::record_response` が dedup 後の数を足すため)。
  食い違うのは `usage.json` が壊れているか、数え方が変わったとき。

## ログの `usage` 行

`usage::record_response` は数えるたびに INFO を 1 行書く (何も返らなかった read を除く)。

```
2026-09-20T01:02:03Z INFO usage list_timeline (posts): returned 20, counted 3, today 952
2026-09-20T01:02:09Z INFO usage create_like (write): counted 1, today 17
```

`counted` は同日 dedup 後の数で、`usage.json` に足された数と同じ。この行がある日は、
作成時刻の近似ではなく行の時刻がそのまま課金時刻になる。**スクリプトはまだこの行を
読まない。** 1 日分の実データが溜まったら、時系列の出どころをこちらへ切り替える。

## 単価と種別は写し

スクリプトの jq の表は `x-api-budget` の `reference/pricing.md` (単価) と
`src/usage/kind.rs` (endpoint と種別の対応) の写し。どちらかを変えたらここも直す。
endpoint を足して表に入れ忘れると、その endpoint は単価不明の write として差額に回る。
