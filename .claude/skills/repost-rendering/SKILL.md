---
name: repost-rendering
description: >-
  twigpui で repost (RT)・引用 (引用 RT)・返信・スレッドの行を触るときに使う。
  RT の行の見た目がおかしい、引用カードだけ様子が違う、
  "@user reposted" の行が出ない・二重に出る、といった症状が対象。
  背景・不透明度・テーマの色・余白のようにウィンドウ全体へ効く見た目を
  足す・変えるときにも引く。行の中に埋め込まれた post の面が取り残される
  (#267 の透過がそうだった) のを防ぐための点検表がここにある。
  API のレスポンスから repost を展開する経路そのものは x-api-endpoints が担当。
---

# repost-rendering

RT の行は「普通の行」ではない。本文は元 post のもの、行の id は RT 自身のもの、
そして元 post が引用なら引用カードまで付いてくる。この重なりが、ウィンドウ全体に
効かせたつもりの変更を取り逃がす場所になる。

## 言葉

| 画面で言うもの | このクレートの言葉 | どこで決まるか |
| --- | --- | --- |
| RT | repost (`reposted_by`) | `x_api/model.rs` の `build_item` が `retweeted` 参照から展開する |
| 引用 RT | quote (`quoted`) | 同上、`quoted` 参照から |
| 引用の RT | repost で `quoted` も持つ行 | 元 post 自身が引用だったとき。両方を見せる (#13) |
| 返信 | reply (`replied_to`) | 同上、`replied_to` 参照から |

コード・コメント・テスト名は "retweet" ではなく "repost" と綴る (`model.rs` の
`PublicMetrics` の doc)。ワイヤ上の綴り (`retweet_count`、`retweeted`) だけが例外。

## RT の行が描く面

RT に関わる描画は 1 か所に無い。触るときは下の全部を通す。

| 面 | 関数 | ファイル | 呼び出し元 |
| --- | --- | --- | --- |
| "· @user reposted" のラベル | `repost_banner_label` | `ui/render/post.rs` | `post_row` の meta 行 |
| 引用カード | `quote_card` | `ui/render/post.rs` | `post_row` (引用と引用の RT)、`composer_quote_card`、`composer_reply_card` |
| スレッドの親 | `thread_row` (`render_thread_chain` 経由) | `ui/render/post.rs` | `post_row` → `thread_section` |
| Repost ボタンの on/off | `repost_button` / `repost_state_for` | `ui/action_row.rs` | `action_row` |
| 操作が効く先の id | `action_post_id` | `x_api/model.rs` | like / repost / quote / open |

**埋め込まれた post の面は `quote_card` と `thread_row` の 2 つ。** どちらも
`bg_header` で塗った枠付きのブロックで、`post_row` の本文の *中* に座る。
`layout.rs` の root・toolbar・status bar は枠 (chrome) の面で、この 2 つは
そこに含まれない。

`quote_card` は composer からも呼ばれる。post の行だけ直して composer の
カードを忘れると、引用を書き始めた瞬間に古い見た目が戻る。

## ウィンドウ全体に効く見た目を変えるときの点検表

背景の不透明度 (#267)、テーマの色スロット、角丸、余白のように「ウィンドウの
どこでも同じであるべき」ものを足したり変えたりしたら、commit の前に順に見る。

1. `layout.rs` の root、`chrome.rs` の toolbar と status bar。ここまでは
   すぐ目に入る。
2. `quote_card` と `thread_row`。**#267 の透過はここを落とした。** root と
   toolbar だけに `bg_alpha` を渡し、引用カードが不透明の板として残った。
3. `quote_card` の composer 側の 2 呼び出し。
4. `frame.rs` のバナー (`session_notice_banner`、`reload_notice_banner`)、
   `sync_row.rs`、`source_picker_menu.rs`。`bg_header` を塗る面はここにもある。
   RT の話ではないが、同じ変更が同じ理由で取り残す。透過はここまで及んでいる
   (#274 の続き)。残る不透明は `sync_dialog` だけで、覆いの上のモーダルなので
   読みやすさを取ってそのままにしてある。

値の渡し方は `layout.rs` の `render` に倣う。`render` が `window` から 1 回
読み、引数で下へ渡す。`self` に一時的なフィールドを置いて render の途中で
読ませない。古い値が残ったフィールドは 2 つ目の不具合になる。

## 見て確かめる

寸法と有無はテストで押さえられるが、色と不透明度は撮るしかない
(`fixture-visual-check`)。`fixtures/timeline.json` には RT の面を 1 画面に
並べてある。

| 行 | 何を見せるか |
| --- | --- |
| 画像を持つ repost | 元 post のメディアが外側の行に出る (#104) |
| 画像を持つ引用 | 引用カード自身のメディア (#123) |
| 引用の repost | "@user reposted" の行と引用カードが同じ行に共存する (#13)。埋め込まれた面を一番多く持つ行 |
| 返信 | "Show thread" のトグル。押した先の `thread_row` は fixture では描けない |

透過の見た目を撮るなら、fixture に `"translucent": true` を書く。fixture の
ウィンドウは window state ファイルを読まないので、書かなければ常に不透明。
撮るときは `cleanshot-capture area` で座標を指定し、後ろに文字のある窓を
置いておく。`window` で撮ると alpha が背景無しで写り、透けているかが分からない。
ウィンドウは手元に無いときだけ透けるので、撮る前に別のアプリを前に出しておく。

透けた面が重なる場所は、1 枚ずつ 70% でも合成後は約 91% になる。引用カードが
周りの本文より濃く見えるのはこの重なりで、不具合ではない。濃さを揃えるかは
別の判断。

## 足すときの型

- 行に新しい面を足す (バッジ、カード、帯) なら、`quote_card` と同じく
  `render/post.rs` の自由関数にして `post_row` から呼ぶ。`post_row` の中に
  直接 builder chain を書くと、composer から再利用できず 2 つ目が生まれる。
- 元 post と RT 自身のどちらの値を読むかは `TimelineItem` の doc が
  フィールドごとに言っている (`id` は RT 自身、`text`・`media`・`metrics` は
  元 post)。迷ったら `model.rs` の `build_item` を読む。
- RT に効く変更は、`a_repost_of_a_quote_carries_the_nested_quote_card`
  (`model.rs`) と `offers_quote_on_a_repost_row` / `offers_like_on_a_repost_row`
  (`ui/mod.rs`) が既存の守り。描画の面を足したら `fixture::tests` の
  「同梱の fixture がその行を持っている」assert も足す。
