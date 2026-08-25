# タイムライン: ウィンドウが何を見せ､どう組み立てているか

このアプリは自分のホームタイムラインを — `GET /2/users/me` が解決する id に
対する `GET /2/users/:id/timelines/reverse_chronological` を — リロード
ボタンの付いたスクロール可能なリストで表示する｡"Load older" ボタンは
`meta.next_token` を使ってさらに過去へページングする｡

**認証手段は X でのサインインだけになった** (#33)｡app-only の bearer token は
削除した｡ホームタイムラインを読めず､投稿・repost・引用・like・削除も
できなかった — このアプリがやることのほとんどだ｡使っていたなら
[Migrating from the bearer token](../README.md#migrating-from-the-bearer-token) を参照｡

`--fetch-only` は今も `X_TARGET_USERNAME` の post を
`GET /2/users/:id/tweets` 経由で取得する｡このエンドポイントは OAuth
トークンでも問題なく動くし､app-only の*認証情報*を落とすことは､
単一ユーザーの*ビュー*を落とすことと同じではない｡

**repost と引用は展開する (#13)｡** API の生のレスポンスは repost を
`RT @user: …` に切り詰める｡そこで両方の timeline リクエストは
`referenced_tweets` も要求するようにした
(`expansions=referenced_tweets.id,referenced_tweets.id.author_id` と
`tweet.fields=referenced_tweets`)｡参照先の post の本当のテキストと著者が
同じレスポンスで返ってくるので､リクエストの追加コストはかからない｡
repost は元の著者と全文の上に小さな "`@user reposted`" の行を付けて描画し､
引用は引用先の post を自分のテキストの下に枠付きのカードとして埋め込む｡
引用の repost は両方を見せる — 元の著者のテキストを本文として､そして
元の post 自身が持っていた引用カードを｡参照先の post が削除済み・
非公開・その他の理由でレスポンスの `includes` に無いときも､行は描画される —
repost は API 自身の (切り詰められているかもしれない) テキストへ
フォールバックして著者は空のままになり､引用はカードを省くだけだ｡

**返信の文脈と "Show thread" (#12)｡** 返信には "Replying to @someone" が
無料で出る — 上記の `referenced_tweets` expansion のおかげで､親の著者は
同じレスポンスの `includes` にすでに入っているので､追加のリクエストコストは
かからない｡ただし会話をさらに上へ辿るのは実際に金がかかるので､自動では
決して行わない｡代わりに各返信は "Show thread (up to 5 requests)" のトグルを
出し､クリックする前に最悪のケースを明示する｡クリックすると親の連なりを
1 階層につき `GET /2/tweets?ids=` 1 リクエストで辿る — 各階層の id は前の
階層が解決してはじめて分かるので､まとめて 1 回にはできない — そして 5 階層に
達するか､最初に空で返ってきた親 (削除済み・非公開・その他の理由で不在) の
どちらか早いほうで止まる｡上限に達したことは､スレッドが黙って途切れるのでは
なく明示的に報告し ("Reached the 5-level limit…")､取得がエラーになったときは
その場で再試行を出す｡最初の親が見つからないときは､空白を空けるのではなく
"The parent post is no longer available" を描画する｡一度辿ったスレッドは
キャッシュされる (`thread-<reply_id>.json`､[Local cache](../.claude/skills/x-api-budget/reference/app-behavior.md#local-cache) を参照)｡
なので同じ返信のスレッドを開き直すのは — アプリを再起動した後でも — それ以上
何もかからない｡post *への*返信を一覧する (逆方向) には別のエンドポイント
(`search/recent`) が要るのでここでは対象外 — #36 を参照｡

**返信・repost・like の件数 (#67)｡** 各 post は本文の下の目立たない行に､
どれだけ反応を得たかを表示する — `12 replies · 34 reposts · 5.6K likes`｡
上の返信の文脈と同じで､これも追加のリクエストコストはかからない｡両方の
timeline リクエストが `tweet.fields` に `public_metrics` を足すだけなので､
数字はすでに支払い済みのレスポンスの中に届く｡0 の件数は省き､反応がまったく
無い post には行そのものを出さない｡取得したての timeline が 0 の壁に
ならないようにするためだ｡大きな数字は略記して (`12.3K`, `2.4M`)､人気の
post が byline を押し広げないようにする｡

これらは **post を取得した時点のスナップショット**であって､更新する仕組みは
無い｡リロードは手元にある最新のものより新しい post を要求する (`since_id`)
ので､すでにキャッシュにある行が再び返ってくることはなく､届いたときの件数を
持ち続ける｡repost は*元の* post の件数を表示する｡本文も元の post の
テキストなので､それと揃う｡

`--fetch-only` は同じ取得をヘッドレスで実行し (認証情報にかかわらず常に
単一ユーザーのビュー)､post を出力する｡ウィンドウを開かずに認証情報を
確かめるのに便利だ:

```sh
cargo run -- --fetch-only
```

**特定の post を 1 件だけ取得する: `--fetch-post` (#42)｡** 欲しいものが
timeline ではなく､どこかから参照された単一の post であることもある —
たとえば Claude Code のセッションに post のテキストを読ませたいときだ｡
`x.com` 自身は `WebFetch` に 402 を返すので､そうしないと人間が手で
テキストを貼り付けることになる｡`--fetch-post` は post の id､完全な
status URL (`https://x.com/<user>/status/<id>` または `twitter.com` の
エイリアス)､あるいはそのどちらかをカンマ区切りで並べたものを受け取り､
取得した post を JSON として stdout に出力する — ウィンドウも人間向けの
表示モードも無い｡出力を読むのがツールだからで､これは `--usage` が自分の
出力にすでに適用しているのと同じ理屈だ:

```sh
cargo run -- --fetch-post 1700000000000000001
cargo run -- --fetch-post https://x.com/jack/status/20
cargo run -- --fetch-post 20,30,40
```

すべての id は 1 回の `GET /2/tweets?ids=` リクエストに入る — X 自身の
クエリパラメータがすでにカンマ区切りのリストを受け付けるので､複数の post を
まとめて取得してもコストはちょうど **1** リクエストで済む｡これは､要求した
id のうち実際に何件返ってきたか (欠けているものはたいてい削除済みか非公開)
と一緒に stderr へ報告する｡出力される post はどれも､timeline 自身が join
しているのと同じ repost/引用/返信の文脈
(`reposted_by`/`quoted`/`replied_to`) を持つ (#12, #13)｡追加のリクエスト
コストはかからない｡

`--fetch-post` は timeline のキャッシュ (#9) に一切触れない — 読みもしないし
書きもしない｡あのキャッシュは､同じアカウントの timeline をリロードのたびに
取得し直さないために存在する｡任意の post id はその「繰り返しアクセスする」
性質を持たない — たいていはリンクされた場所から 1 回引かれるだけだ｡だから
最も単純で筋の通る選択は､常に 1 リクエストを使い､結果を永続化しないこと
になる｡

