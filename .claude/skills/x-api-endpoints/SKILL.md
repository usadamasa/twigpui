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

`expansions` が受け付ける値も同じ規則で切り替わる。

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
3. **それでもまだ足りない** — `expansions=referenced_tweets.id` は 400 の列挙に
   載っていないのに 200 で通り、`includes.tweets` を返す。
   ドット付きのパス表記は列挙外でも受け付けられる。

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

エラー本文は RFC 7807 風の `{type, title, detail, errors[]}`。
`type` は `https://api.x.com/2/problems/...`。

- **404 ではなく 400 が返る場面がある。** 存在しない username を引くと 404 ではなく
  400 で、しかも理由は「存在しない」ではなく正規表現 `^[A-Za-z0-9_]{1,15}$` 不一致。
  username の 15 文字上限は spec のパラメータ制約に書かれていない。
- **部分成功は 200 で返る。** `GET /2/tweets?ids=` に実在 id と不存在 id を混ぜると
  200 が返り、`data` に取れた分だけ、`errors[]` に `Not Found Error` が入る。
  `meta.result_count` は返った分しか数えない。**`errors[]` を見ないと欠落に気づけない。**
- **403 の文面は誤解を招く。** `/2/tweets/analytics` の 403 は
  「Project に紐づいた App のキーを使え」(`reason: client-not-enrolled`) と言うが、
  同じトークンで他の v2 エンドポイントは 200 を返す。本当の意味は
  `required_enrollment` のほう、つまりアクセス階層不足。
  文面どおりに App 設定を疑うと時間を溶かす。
- **エラー本文は送ったパラメータをエコーバックする。** `ids` に 101 件送った 400 の
  本文には 101 件全部が入る。そのままログへ出すと膨れる。
- 429 は 2 種類ある。レートリミットと `UsageCapExceeded` (残高切れ) の区別は
  `x-api-budget` を見る。

## レートリミットはエンドポイントごとに桁が違う

`x-rate-limit-limit` レスポンスヘッダは**どのリクエストにも付いてくる**ので、
上限を知るための追加リクエストは要らない。実測値は 10 から 5000 まで開いている
(403 を返した `/2/tweets/analytics` だけは 40000 と申告してくる)。

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

`x-access-level` ヘッダも毎回付く。App の権限 (`read-write` など) が
開発者コンソールを開かずに分かる。

## スコープと到達範囲

committed scope は `src/oauth/pkce.rs` の `SCOPES`:
`tweet.read users.read tweet.write like.write offline.access`。
この scope で届く GET は spec 上 26 本で、到達可否は `reference/endpoints.md` にある。

**`GET /2/users/{id}/following` は届かない。** `follows.read` が要る。
手元の live token がこの scope を持っていることがあるが、それは過去の再認可の名残で、
committed code は要求していない。**次の再認可で黙って消える。**
`/following` に依存するコードや probe を書くと、そのときに落ちる。

## 実測の作法

課金が発生する調査には 2 つの規則がある。

**仮説は、別の仮説と区別できる計測が出るまで閉じない。**
#157 の調査では「スコープ不足」という仮説に対して `follows.read` を足す修正を入れ、
再認可を通しても症状が変わらないことを確認しないまま先へ進みかけた。
spec の `security` ブロックがそれを反証した。
「直したら直った気がする」は計測ではない。

**観測は撃った実行の中でファイルへ落とす。**
端末が流れたせいで同じリクエストを撃ち直すのは、そのまま二重課金になる。

probe crate の組み方 (Bash からトークンが読めない制約への対処を含む) は
`reference/probing.md`。

## テスト

**テストはネットワークを叩かない。** ここで実測した挙動をテストに落とすときは、
probe が保存した生 JSON をフィクスチャに使う。
テストが課金を発生させないことを保ちつづける。
