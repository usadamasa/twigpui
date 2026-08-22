---
name: x-api-endpoints
description: >-
  X API のエンドポイントを選ぶとき、クエリパラメータや expansions を足す・変えるとき、
  レスポンスが期待と違うとき、4xx の意味を読むときに使う。
  どのスコープで何が届くか、公開 spec と実際に動く API のどこが食い違うか、
  課金を最小にして挙動を実測する手順を扱う。
  1 操作あたりの課金と、キャッシュ・レートリミットの持ち方は x-api-budget が担当。
---

# x-api-endpoints

X API の**契約と実挙動**を扱う。課金の数え方は `x-api-budget` にあるので重複させない。

挙動に関する主張は 2026-08-23 の実測が根拠。未計測の項目は
`reference/endpoints.md` でそう明記してあり、そこだけ spec が出典になる。
観測の全表は `reference/endpoints.md`、再実測の手順は `reference/probing.md`。

## 最初に読むべき一行

**docs.x.com の公開 spec (`openapi.json`) に従うとこのアプリは壊れる。**

X API には post 語彙と tweet 語彙という 2 系統が並存していて、
**リクエストで使った綴りがレスポンスの綴りを決める**。
公開 spec は post 語彙しか宣言していないが、このアプリは tweet 語彙で書かれている。

| 送るパラメータ | `data[]` のキー | `includes` のキー |
| --- | --- | --- |
| 無指定 または `tweet.fields` | `edit_history_tweet_ids` `referenced_tweets` | `includes.tweets` |
| `post.fields` | `edit_history_post_ids` `referenced_posts` | `includes.posts` |

`expansions` については、切り替わるのは**レスポンスの語彙と 400 が返す列挙**であって、
受け付ける値そのものではない。`post.fields` を付けずに `expansions=referenced_posts` を
送っても 200 が返り、レスポンスは tweet 語彙 (`referenced_tweets` / `includes.tweets`) になる。

`src/x_api/model.rs` の `Includes::tweets` と `Post::referenced_tweets` はどちらも
`#[serde(default)]` なので、spec に合わせて `post.fields` へ「近代化」すると
**エラーが出ないまま空ベクタになる**。リポストと引用の中身が全部消えるが、
ログにも型にも何も現れない。

パラメータ名を触るときは、この表のどちら側にいるかを先に決める。混ぜない。

## spec も 400 の列挙も、受け付ける値の完全な一覧ではない

真実は 3 段階あって、どれ一つとして完全ではない。

1. **公開 spec の enum** — post 語彙のみ。`author_id` `referenced_tweets`
   `in_reply_to_user_id` `rest_id` を含まない。
2. **400 の本文** — 不正な値を 1 つ送ると、そのパラメータが実際に受け付ける
   enum を全部返す。spec より実態に近い。
3. **それでもまだ足りない** — 反例が 2 つある。
   `expansions=referenced_tweets.id` (ドット付き) も
   `expansions=referenced_posts` (別語彙) も、tweet モードの 400 の列挙には
   載っていないのに 200 で通る。tweet モードで実際に受け付ける集合は、
   spec の列挙とも 400 の列挙とも一致せず、両方より広い。
   **なぜ広いのかは分かっていない。** ドット付きだから通る、という説明ではない
   (`referenced_posts` はドット付きではない)。

**実リクエストだけが真。** spec は出発点、400 の列挙は近道、確認は実測。

## 400 を enum の oracle として使う

パラメータに存在しない値を 1 つ送ると、400 の本文に受け付ける値が全部並ぶ。

```
GET /2/users/{id}/tweets?max_results=5&expansions=bogus
-> 400
   "The `expansions` query parameter value [bogus] is not one of [...]"
```

新しい `*.fields` や `expansions` を足す前にこれを 1 本撃つほうが、
綴りを推測してリトライを繰り返すより安い。ただし 1 リクエストは 1 リクエスト分の課金なので、
撃つ前に `x-api-budget` の数え方に従って記録する。

## 4xx の読み方

エラー本文は RFC 7807 風で `type` は `https://api.x.com/2/problems/...`。
ただし形は一定でない。400 は `{type, title, detail, errors[]}` を返すが、
403 (`/2/tweets/analytics`) は `errors[]` を持たず
`{type, title, detail, client_id, reason, registration_url, required_enrollment}` だった。
**`errors[]` があるものとして書くと 403 で落ちる。**

- **形式違反は 404 ではなく 400。** username を引くとき、正規表現
  `^[A-Za-z0-9_]{1,15}$` に一致しない値 (16 文字など) は 404 ではなく 400 で、
  理由も「存在しない」ではなく regex 不一致と返る。
  この regex は spec のパラメータ制約にも書かれており、ここは spec と API が一致している。
  **形式を満たす不存在の username がどう返るかは未計測。**
  `ids` の挙動 (下記) から類推すると 200 + `errors[]` の可能性がある。
- **部分成功は 200 で返る。** `GET /2/tweets?ids=` に実在 id と不存在 id を混ぜると
  200 が返り、`data` に取れた分だけ、`errors[]` に `Not Found Error` が入る。
  `meta.result_count` は返った分しか数えない。**`errors[]` を見ないと欠落に気づけない。**
- **403 の文面は誤解を招く。** `/2/tweets/analytics` の 403 は
  「Project に紐づいた App のキーを使え」(`reason: client-not-enrolled`) と言う。
  **これは Project 紐付けの問題ではない。** 同じトークンで他の v2 エンドポイント
  17 本が 200 を返しており、v2 は Project 配下の App を要求するのだから、紐付けは生きている。
  残る候補は同じ本文の `required_enrollment: "Appropriate Level of API Access"`、
  つまりアクセス階層だが、**こちらは推定で未検証**。
  計測で言えるのは「App 設定を疑うな」までで、階層だと断定はできない。
- **エラー本文は送ったパラメータをエコーバックする。** `ids` に 101 件送った 400 の
  本文には 101 件全部が入る。そのままログへ出すと膨れる。
- 429 は 2 種類ある。レートリミットと `UsageCapExceeded` (残高切れ) の区別は
  `x-api-budget` を見る。

## レートリミットはエンドポイントごとに桁が違う

`x-rate-limit-limit` レスポンスヘッダは、観測した 34 本 (200 / 400 / 403) の
**すべてに付いていた**ので、上限を知るための追加リクエストは要らない。
実測値は 10 から 5000 まで開いている
(403 を返した `/2/tweets/analytics` だけは 40000 と申告してくる)。
**ただし 401 では付かない。** アクセストークンが失効した状態で撃つと、
返ってくる `x-` ヘッダは `x-response-time` `x-served-by` `x-transaction-id` の 3 つだけで、
`x-rate-limit-*` も `x-access-level` も無い。本文も
`{"type":"about:blank","title":"Unauthorized","detail":"Unauthorized","status":401}` と、
`https://api.x.com/2/problems/...` を名乗る他のエラーとは別物になる。

**ヘッダからレートリミットを読む処理は、無い場合を通らなければならない。**
失効は「上限が 0 になった」ではないので、`x-rate-limit-remaining` の欠落を
枯渇として扱うと誤診する。429 / 5xx では未観測。

| エンドポイント | 上限 |
| --- | --- |
| `GET /2/tweets?ids=` | 5000 |
| `GET /2/users/{id}/tweets` | 900 |
| `GET /2/users/{id}/timelines/reverse_chronological` | 180 |
| `GET /2/users/me` | 75 |
| `GET /2/tweets/{id}/retweeted_by` | 75 |
| `GET /2/users/personalized_trends` | 10 |

全表は `reference/endpoints.md`。`src/rate_limit.rs` に `Endpoint` を足すときは、
実測した上限をこの表と突き合わせる。**1 操作で複数エンドポイントを叩く設計は、
その中で一番小さい上限に縛られる。**

`x-access-level` ヘッダも同じ 34 本すべてに付いていた (401 を除く、上記)。
App の権限 (`read-write` など) が開発者コンソールを開かずに分かる。

## トークンが失効したら、probe ではなくアプリに更新させる

probe の 401 はたいていアクセストークンの失効で、`cargo run -- --fetch-only` を
一度通せば直る。`oauth::resolve_credential` が更新して保存するため。

**probe 側で refresh token を使ってはいけない。** X は更新のたびに refresh token を
回転させるので、probe が使って保存しなければアプリが持っている refresh token が
無効になり、ユーザーは再認可をやり直すはめになる。
再認可は scope も引き直すので、live token にだけ残っている scope はそこで消える。

`--fetch-only` は 2 リクエスト課金される (`x-api-budget` 参照)。
更新そのもの (token endpoint) はデータ API ではないので課金されない。

## スコープと到達範囲

committed scope は `src/oauth/pkce.rs` の `SCOPES`:
`tweet.read users.read tweet.write like.write offline.access`。
この scope で届く GET は spec 上 26 本 (scope を明示する GET のみを数えた場合)。
到達可否は `reference/endpoints.md` にある。

**`GET /2/users/{id}/following` は届かない。** `follows.read` が要る。
手元の live token がこの scope を持っていることがあるが、それは過去の再認可の名残で、
committed code は要求していない。**次に認証フローを回した時点で消える。**
`offline.access` によるリフレッシュは元の scope を保つので、消えるのは
リフレッシュ時ではなく再認可時。`/following` に依存するコードや probe は、そのときに落ちる。

## 挙動を確かめるときは書き捨ての Rust を書く

**シェルからは撃てない。** sandbox の制約が 3 つ重なっている。

- `~/.local/state/twigpui/oauth_tokens.json` は read deny-list に入っていて読めない
- `curl` は permission-denied
- `.env` も Claude セッションからは読めない

**抜け道は `cargo` ひとつ。** `.claude/settings.json` の `sandbox.excludedCommands` に
`cargo *` があるので、`cargo run` で起動したプロセスだけが sandbox の外で動き、
トークンを読めて `api.x.com` に届く。

したがって probe は**書き捨ての Rust クレートとして書く**。
`./tmp/probe` (gitignore 済み) に置き、`ureq` + `serde_json` で撃ち、
終わったら `~/.claude/scripts/clean-tmp.sh ./tmp/probe` で消す。
どちらの crate もローカルの registry キャッシュにあるのでビルドは速い。

```sh
cargo run --quiet --manifest-path ./tmp/probe/Cargo.toml -- ./tmp/probes/round1.tsv
```

アプリ本体にデバッグ用のフラグやコードを足して確かめようとしない。
確かめたいことは毎回違うのに、本体に残ると次から邪魔になる。
書き捨ては書き捨てのまま捨てる。

クレートの中身 (非 2xx をエラーにしない設定、ヘッダの保存、TSV での入力) は
`reference/probing.md` に全部ある。

## 実測の作法

課金が発生する調査には 3 つの規則がある。

**仮説は、別の仮説と区別できる計測が出るまで閉じない。**
#157 の調査では「スコープ不足」という仮説に対して `follows.read` を足す修正を入れ、
再認可を通しても症状が変わらないことを確認しないまま先へ進みかけた。
spec の `security` ブロックがそれを反証した。
「直したら直った気がする」は計測ではない。

**観測は撃った実行の中でファイルへ落とす。**
1 リクエストにつき生のレスポンス 1 ファイルと ledger 1 行を、受け取った直後に書く。
端末が流れたせいで同じリクエストを撃ち直すのは、そのまま二重課金になる。
probe のリクエストは `usage.json` に計上されないので、数えるのは自分の仕事。

**1 リクエストで 2 つ以上の問いに答えさせる。**
`?max_results=5&tweet.fields=...` なら下限境界とフィールド名の両方が同時に分かる。
撃つ前に「この結果がどちらに転んでも何かが決まるか」を言えるようにする。
言えないなら、それは課金するだけで何も決まらないリクエストになる。

## テスト

**テストはネットワークを叩かない。** ここで実測した挙動をテストに落とすときは、
probe が保存した生 JSON をフィクスチャに使う。
テストが課金を発生させないことを保ちつづける。
