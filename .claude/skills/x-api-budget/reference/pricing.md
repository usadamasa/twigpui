# X API の課金仕様 — 単価・出典・実測

`../SKILL.md` が判断の規範、こちらが数字と根拠。

## 出典と、更新の仕方

**正本は `https://docs.x.com/x-api/getting-started/pricing`。**
下の表は 2026-08-23 に読んだ内容で、X は予告付きで改定する
(直近では 2026-04-20 に Owned Reads を $0.001 へ変更している)。

更新するとき:

1. 上の URL を読み直して表を差し替える
2. 表の直後の「最終確認」の日付を書き換える
3. 単価が変わったら `../SKILL.md` の「1 操作あたりの見込み」の桁が動くので、そちらも見る
4. 改定の告知は `https://devcommunity.x.com` の Announcements に出る

**金額はドキュメント記載であって実測していない。** 実測したのは「件数で課金される」
という単位のほう (下の「実測ログ」)。例外は `List member: Add` で、docs に無いので残高の減りから
出した (実測ログ 6)。金額まで突き合わせたいなら Developer Console の
Usage / Billing の明細を見る。API からは金額が取れない。

## 単価

最終確認: **2026-08-23**

| 種別 | 単価 |
| --- | --- |
| Posts | $0.005 / resource |
| Users | $0.010 / resource |
| DM Events | $0.010 / resource |
| Following / Followers | $0.010 / resource |
| Lists | $0.005 / resource |
| Spaces | $0.005 / resource |
| Communities | $0.005 / resource |
| Notes | $0.005 / resource |
| Likes | $0.001 / resource |
| Mutes / Blocks | $0.001 / resource |
| **Owned Reads** | $0.001 / resource |
| Post: Create | $0.015 / request |
| Post: Create (URL 付き) | $0.200 / request |
| DM Interaction: Create | $0.015 / request |
| List: Create | $0.010 / request |
| List member: Add | $0.010 / request (**docs に無い。2026-09-21 の実測**、実測ログ 6) |

**Owned Reads** は「自分の App から自分のデータを読む」もの。対象として
自分の投稿・メンション・いいね・ブックマーク・フォロワー・フォロー・ブロック・ミュート・
リストが挙げられている。ホームタイムラインとリストのタイムラインは**他人の投稿**なので
Posts 単価。

## 課金の単位

> All prices are per resource fetched (reads) or per request (writes/actions).

> Different posts in the same request = each counts separately

読み取りは**返ってきたオブジェクトの数**。書き込みはリクエスト数。

## 重複排除

> All resources are deduplicated within a 24-hour UTC day window.
> If you request and are charged for a resource (such as a Post), requesting
> the same resource again within that window will not incur an additional charge.

窓は UTC の日境界で、`src/usage/mod.rs` が使っている日境界
(`unix_seconds.div_euclid(86_400)`) と同じ。

失敗したリクエストは課金されない ("Only successful responses that return data are billed")。

## 月次の上限

pay-per-use は **月 300 万 Post reads**。`GET /2/usage/tweets` の `project_cap` が返す。
残高切れの `429 UsageCapExceeded` とは別物。

## 消費を読む — `GET /2/usage/tweets`

project 単位の **Post 消費**を返す。Users やその他の resource は出てこない。

```
GET https://api.x.com/2/usage/tweets
    ?days=1
    &usage.fields=project_usage,project_cap,cap_reset_day,daily_project_usage,daily_client_app_usage
```

- **OAuth 2.0 Application-Only 専用。** ユーザーコンテキストのトークンでは 403
  (`"Supported authentication types are [OAuth 2.0 Application-Only]"`)
- このエンドポイント自体は Post として課金されない (2 回続けて撃って値が動かないことを確認)
- `daily_client_app_usage` で App 別に割れる。複数の App が同じ project にいるとき、
  どちらが使ったかが分かる
- 反映は即時ではないことがある。app-only Bearer で自分の投稿を引いた分は
  3 分待っても現れなかった (下のログ 2)

## 実測ログ

すべて 2026-08-23、project `2088222118760579074`。

### 1. per resource であることの確認

新しく作った App が 1 日のうちに撃ったのは 2 本だけ
(`GET /2/users/me` と `max_results=20` のホームタイムライン、返却 20 件)。

```
daily_client_app_usage  client_app_id 33345087  →  20
```

per request なら 1 か 2。**20 は返却件数そのもの。**
`GET /2/users/me` はこのカウンタに乗っていない (Users は Post カウンタの対象外)。

### 2. app-only Bearer での読み取りが乗らなかった件 (未解決)

`GET /2/users/5685672/tweets?max_results=10&end_time=2026-08-10T00:00:00Z` を
app-only Bearer で撃ち、10 件が返った。直後も 3 分後もカウンタは動かなかった。

説明の候補が 2 つあり、**区別できていない**。

- app-only の読み取りが別勘定になっている
- 自分の投稿を引くのが Owned Reads 扱いで、Post cap のカウンタに乗らない

どちらであっても下の 3 の結論は動かない。

### 3. 決定的な観測

```
GET /2/lists/2091351590695588200/tweets?max_results=100
  → result_count 98
  usage  20  →  119   (Δ 99)
```

リクエストは 1 本。返却は 98 件。**1 リクエストが 99 resource として課金された**
(差の 1 件は同時刻の別取得で入った新着)。

### 4. `includes` は Post として課金されていない

上の 1 の取得は
`expansions=attachments.media_keys,author_id,referenced_tweets.id,...` 付きで、
`data` 20 件のうち 5 件がリポスト = `includes.tweets` に元投稿が 5 件以上入っていた。
それでも計上は **20**。

3 の取得も `expansions=author_id` で `includes.users` が 2 件返ったが、
Δ は 99 (= 98 + 新着 1) で、users の分は乗っていない。

**`includes.users` が Users 単価 ($0.010) で課金されているかは未解決。**
`GET /2/usage/tweets` は Post 専用なので、この方法では答えが出ない。
コンソールの明細を見るしかない。

### 5. following は Owned、list members は Users。endpoint をまたぐ dedup は無い (2026-09-21)

残高切れの状態で 10:31 JST に $5 を購入し、直後に release の `--sync-list` (dry-run) を 1 回実行した。
この UTC 日にほかの読み取りと書き込みは無い (`usage.json` の posts と write は 09-19 のまま)。

```
following     24 ページ完走、返却 2340 件
list_members  4 リクエスト、途中で 402 (plan は保存されなかった)
```

- following が $0.010 なら 500 件前後で 402 になる。2340 件が完走したので **Owned Reads $0.001** (= $2.34)。
- 残り約 $2.66 が list_members の 3 ページ前後で尽きた。**Users $0.010** と整合する。
  docs の料金ウィジェットが `get-list-members` を Owned の対象外にしているのと同じ読み。
- list の member は全員 following でも返っていた。それでも課金されたので、
  **同じアカウントでも endpoint (種別) が違えば同日に再課金される。**

金額は残高の尽き方からの推定で、Developer Console の明細とは突き合わせていない。
同じ endpoint を同日にもう一度読んだときの dedup は、この観測では分からず、同日午後の 6 で確かめた。

### 6. `add_list_member` は $0.010 / request。同じ endpoint・同じ UTC 日の 2 回目は課金されない (2026-09-21)

#288 のマージ後に release で `--sync-list --reread` と `--sync-list --apply` (10 件で中断) を実行し、
Developer Console の残高と支払い履歴を突き合わせた。

**`add_list_member` の単価。** 10 件送って残高が $23.22 → $23.12。**$0.010 / request** で、
`List: Create` と同額。10 件とも 200 で、400 も 429 も無し (#197 の不透明な 429 は出なかった)。
残高の反映が遅れている可能性は残るので、100 件で $1.00 減るかをもう一度見る。

**同じ endpoint・同じ UTC 日の 2 回目は課金されない (Owned も Users も)。**
この日のチャージは $5 + $5 + $25 = $35 (01:30 / 01:35 / 02:22 UTC)。読んだ resource を
「種別ごとに 1 日 1 回」で数えると:

| | 件数 | 単価 | 金額 |
| --- | --- | --- | --- |
| following (朝に 2,340 件完走、`--reread` でもう一度 2,340 件) | 2,340 | $0.001 | $2.34 |
| list members (朝は約 300 件で 402、`--reread` で 942 件完走) | 942 | $0.010 | $9.42 |
| `/2/users/me?user.fields=public_metrics` | 1 | $0.001 | $0.001 |

$35 − $11.76 = **$23.24**。`--reread` 直後の実残高は **$23.22**。
2 回目も課金されていたなら残高は約 $18 になるので、その仮説は棄却できる。差の $0.02 は未説明
(09-06 の残高切れ時に少しマイナスへ入っていた可能性)。

帰結:

- 読み取りが途中の 402 で落ちても、同じ UTC 日のうちに打ち直せば支払い済みの分は再課金されない。
  計画は残らないが、金は残る
- endpoint をまたぐ dedup が効かないことは変わらない (5)
- アプリの dedup (`(種別, id)` で UTC 1 日 1 回、Following は Owned、ListMembers は Users) は
  X の課金と一致している

**まだ分からないこと。** すでに member のアカウントへの追加が 200 か 400 か。この日の 10 件に該当が無かった。
