# エンドポイント一覧 — 到達範囲・上限・実測値

出典は 2 つ。**spec** は `https://docs.x.com/openapi.json`、
**実測** は 2026-08-23 に OAuth 2.0 ユーザートークンで撃った 34 リクエスト。
両者が食い違うところは実測を採る (理由は `../SKILL.md` の冒頭)。
**実測と書いていない数値は spec が出典**で、そこは検証されていない。

committed scope: `tweet.read users.read tweet.write like.write offline.access`
(`src/oauth/pkce.rs` の `SCOPES`)。

計測に使ったトークンは committed scope に加えて `follows.read` を持っていた
(#157 の調査中に一度だけ再認可した名残)。本表のどのエンドポイントも
`follows.read` を要求しないので結果には影響しないが、
「committed scope ちょうどで撃った」わけではない。

## アプリが使っている 11 本 (`src/rate_limit.rs` の `Endpoint`)

URL の組み立ては `src/x_api/client.rs` の各 builder が正本。

| `Endpoint` | メソッドとパス | 必要な scope | 実測上限 |
| --- | --- | --- | --- |
| `UserLookup` | `GET /2/users/by/username/{username}` | `tweet.read` `users.read` | 900 |
| `Me` | `GET /2/users/me` | `tweet.read` `users.read` | 75 |
| `Timeline` | `GET /2/users/{id}/tweets` | `tweet.read` `users.read` | 900 |
| `HomeTimeline` | `GET /2/users/{id}/timelines/reverse_chronological` | `tweet.read` `users.read` | 180 |
| `TweetById` | `GET /2/tweets?ids=` | `tweet.read` `users.read` | 5000 |
| `CreatePost` | `POST /2/tweets` | + `tweet.write` | 未計測 |
| `CreateRepost` | `POST /2/users/{id}/retweets` | + `tweet.write` | 未計測 |
| `DeleteRepost` | `DELETE /2/users/{id}/retweets/{source_tweet_id}` | + `tweet.write` | 未計測 |
| `CreateLike` | `POST /2/users/{id}/likes` | + `like.write` | 未計測 |
| `DeleteLike` | `DELETE /2/users/{id}/likes/{tweet_id}` | + `like.write` | 未計測 |
| `DeletePost` | `DELETE /2/tweets/{id}` | + `tweet.write` | 未計測 |

書き込み系は実アカウントを変更するため意図的に計測していない。
必要になったら投稿内容とクリーンアップをユーザーに確認してから撃つ。

### `BearerToken` を受け付けないもの (spec 由来・未計測)

多くの読み取りは `OAuth2UserToken` / `UserToken` (OAuth 1.0a) に加えて
`BearerToken` (アプリ専用) も受け付けるが、受け付けないものがある。
以下は spec の `security` ブロックを読んだもので、**Bearer で撃って 401 を確認してはいない**。
このドキュメントが扱う範囲では次の 9 本:

`/2/users/me` `/2/users/{id}/timelines/reverse_chronological` `/2/users/search`
`/2/users/personalized_trends` `/2/communities/search` `/2/tweets/analytics`
`/2/media/analytics` `/2/notes/search/notes_written`
`/2/notes/search/posts_eligible_for_notes`

このリポジトリの資格情報は OAuth 2.0 ユーザーコンテキストだけなので実害はないが、
**「Bearer で試して切り分ける」という手はこれらでは使えない。**
`src/x_api/client.rs` の `fn me()` の doc コメントが同じことを書いている。

## committed scope で届くその他の読み取り

| パス | 実測 | 上限 | `max_results` (spec) |
| --- | --- | --- | --- |
| `GET /2/tweets/{id}` | 200 | 900 | — |
| `GET /2/users/{id}` | 200 | 900 | — |
| `GET /2/users/by` | 200 | 900 | `usernames` 1-100 |
| `GET /2/tweets/search/recent` | 200 | 300 | 10-100 (既定 10) |
| `GET /2/users/{id}/mentions` | 200 | 300 | 5-100 |
| `GET /2/users/search` | 200 | 300 | 1-**1000** (既定 100) |
| `GET /2/communities/search` | 200 | 300 | 10-100 (既定 10) |
| `GET /2/users/{id}/affiliates` | 200 | 250 | 1-**1000** |
| `GET /2/tweets/{id}/quote_tweets` | 200 | 75 | 10-100 (既定 10) |
| `GET /2/tweets/{id}/retweeted_by` | 200 | 75 | 1-100 (既定 100) |
| `GET /2/tweets/{id}/retweets` | 200 | 75 | 1-100 (既定 100) |
| `GET /2/users/personalized_trends` | 200 | 10 | — |
| `GET /2/tweets/analytics` | **403** | 40000 | `ids` 1-100 |
| `GET /2/media/analytics` | 未計測 | — | `media_keys` 1-100 |
| `GET /2/media` | 未計測 | — | — |
| `GET /2/media/{media_key}` | 未計測 | — | — |
| `GET /2/users` | 未計測 | — | — |
| `GET /2/news/{id}` | 未計測 | — | — |
| `GET /2/news/search` | 未計測 | — | — |
| `GET /2/notes/search/notes_written` | 未計測 | — | — |
| `GET /2/notes/search/posts_eligible_for_notes` | 未計測 | — | — |

`/2/media/analytics` は必須の `media_keys` が手元に無いため撃っていない。
残りは twigpui の用途から遠いので撃っていない。

この 26 本という数え方は「`OAuth2UserToken` に scope を明示する GET」に限っている。
`OAuth2UserToken: []` (scope の指定なし) の GET が別に 2 本あり
(`/2/trends/by/woeid/{woeid}` `/2/tweets/counts/recent`)、
「空 = 任意のユーザートークンで可」と読むならこれらも届く。どちらも未計測。

### 届かないもの

`GET /2/users/{id}/following` と `GET /2/users/{id}/followers` は `follows.read` を要求する。
committed scope には無い。ブックマークは `bookmark.read`、リストは `list.read`、
いいね一覧は `like.read` で、いずれも同様に届かない。

手元の live token が `follows.read` を持っていることがある。
これは #157 の調査中に反証された仮説のもとで一度だけ再認可した名残で、
committed code は要求していない。**次に認証フローを回した時点で消える。**

## `max_results` の下限は 3 通りある

これが一番踏みやすい。**下の表はほとんどが spec 由来**で、実測は 2 か所しかない。

| 下限 | エンドポイント | 出典 |
| --- | --- | --- |
| 1 | `timelines/reverse_chronological` | **実測** (`max_results=1` が 200) |
| 1 | `retweeted_by` `retweets` `users/search` `affiliates` | spec |
| 5 | `users/{id}/tweets` | **実測** (`max_results=4` が 400、本文が「5 と 100 の間」) |
| 5 | `users/{id}/mentions` | spec (5 が通ることは確認、4 は未計測) |
| 10 | `search/recent` `quote_tweets` `communities/search` | spec (10 が通ることは確認、9 は未計測) |

上限はどれも 100 (`users/search` と `affiliates` だけ 1000)。**これは全部 spec 由来。**
上限側を境界まで撃ったのは `ids` の 1-100 だけで、そこは 101 個で 400 を確認している。

ホームタイムラインと `users/{id}/tweets` は見た目がよく似ているのに下限が違う。
範囲外は 400 で、本文が範囲を教えてくれる。

```
"The `max_results` query parameter value [4] is not between 5 and 100"
```

**範囲を確かめたいときは範囲外を 1 本撃つ。** 本文が下限と上限を両方言う。

## 受け付ける値の実測 (2026-08-23)

400 の本文から取った実際の enum。**公開 spec の enum とは中身が違う。**

`tweet.fields` — spec の `post.fields` enum に無い値を含む。
綴りの違いを除いて本当に spec 側に存在しないのは
`rest_id` `author_id` `in_reply_to_user_id` `username` `edit_history_*` `referenced_*` の各項:

```
id text edit_history_tweet_ids withheld rest_id created_at author_id conversation_id
in_reply_to_user_id referenced_tweets attachments lang possibly_sensitive paid_partnership
reply_settings source display_text_range card_uri community_id note_tweet scopes username
suggested_source_links suggested_source_links_with_counts matched_media_notes
note_request_suggestions public_metrics context_annotations entities geo edit_controls
media_metadata non_public_metrics organic_metrics promoted_metrics article
```

`post.fields` — 同じ内容の post 語彙版。`edit_history_post_ids` `referenced_posts`
`note_post` `article_title` に置き換わる。

`expansions` (tweet 語彙のとき):

```
edit_history_tweet_ids author_id in_reply_to_user_id referenced_tweets
attachments.media_keys attachments.poll_ids attachments.media_source_tweet username
entities.mentions.username geo.place_id article.cover_media article.media_entities
```

**この列挙も完全ではない。** 反例を 2 つ実測している。

- `expansions=referenced_tweets.id` — 列挙に無いが 200、`includes.tweets` を返す
- `expansions=referenced_posts` — 別語彙で列挙にも無いが 200、
  レスポンスは tweet 語彙 (`referenced_tweets` / `includes.tweets`) で返る

つまり tweet モードで受け付ける集合は、spec の列挙とも 400 の列挙とも一致せず、
**両方より広い。理由は分かっていない。**

アプリが使っている `referenced_tweets.id.author_id` と
`referenced_tweets.id.attachments.media_keys` も列挙に無いが、これらは probe ではなく
**`src/x_api/client.rs` の `TIMELINE_FIELDS` が実行時に動いていることが根拠**。

`user.fields`:

```
id name username withheld created_at description entities location pinned_tweet_id
profile_banner_url profile_image_url protected public_metrics url verified
subscription_type verified_type most_recent_tweet_id is_identity_verified affiliation
connection_status receives_your_dm verified_followers_count parody subscribes_to_you
subscription confirmed_email
```

`media.fields`:

```
media_key type url preview_image_url width height alt_text duration_ms variants
public_metrics organic_metrics promoted_metrics non_public_metrics
```

## ホームタイムラインの既知の異常

このアカウントでは 2026-08-16T08:20Z 以降、
`GET /2/users/{id}/timelines/reverse_chronological` がフォロー先の投稿を
サーバ側で返さなくなっている。詳細と切り分けは issue #157。

**このエンドポイントで「何が返るか」を計測しても、それはこのアカウントの異常を
測っているだけになる。** 構造 (パラメータが通るか、`meta` の形、ヘッダ) は採ってよいが、
内容に関する結論は出せない。パラメータの挙動を確かめたいときは
`GET /2/users/{id}/tweets` (自分のタイムライン) を使う。こちらは決定的に動く。

同じトークンで `GET /2/tweets/search/recent` は正常に動く。
ただし計測したのは `from:usadamasa` (自分自身) だけで、
**フォロー先など他人のアカウントに対して同じように引けるかは未計測**。
代替経路として使えるかを判断するにはそこを撃つ必要がある。
クエリ文字列に長さ上限があるため、いずれにせよ多数のフォロー先を
一括で追う用途には向かない。

## 取得範囲 (docs.x.com の散文より・未確認)

`timelines/reverse_chronological` の取得可能範囲は直近 7 日または最新 800 件、
どちらか先に尽きたほうで止まる、とされる。

**この数字の出典は `openapi.json` ではなく docs.x.com の散文ページ**で、
spec のどこにも 7 日も 800 も現れない。ページングで末尾まで辿った計測もしていない。
このドキュメントの他の記述と違い、一次資料で裏を取れていない項目として扱う。
