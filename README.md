<p align="center">
  <img src="assets/AppIcon.png" alt="twigpui" width="180" height="180">
</p>

<h1 align="center">twigpui</h1>

<p align="center">
  Rust と <a href="https://crates.io/crates/gpui">gpui</a> で書いた
  開発用途専用の X (Twitter) タイムラインビューア｡<br>
  macOS 専用 — 他のプラットフォームは考慮しない｡
</p>

## 何ができるか

- ホームタイムラインをスクロールできるウィンドウに表示する｡リロードボタンと､
  さらに過去へ遡る "Load older" ボタンがある｡
- repost と quote をインラインで展開するので､repost は切り詰められた
  `RT @user: …` ではなく元の本文を表示する｡
- リプライの文脈 ("Replying to @someone") を追加コストなしで表示し､
  親チェーンを遡る "Show thread" をオプトインで提供する｡押す前に､
  最悪ケースで何リクエストかかるかを明示する｡
- リプライ・repost・いいねの数を取得時点のスナップショットとして表示する｡
- 投稿・返信・引用・repost・いいね・削除ができる｡
- 添付画像と投稿者のアバターを表示する｡
- 投稿・投稿者・内容を埋めたコンポーザーをブラウザで開く｡
- ヘッドレスで動く: `--fetch-only` は単一ユーザーの投稿を､`--fetch-post` は
  1 つ以上の投稿を JSON で､`--usage` はここまでの API のコストを出力する｡
- `--fixture fixtures/timeline.json` でアカウントではなくファイルから
  ウィンドウを開く — 認証情報もリクエストも不要で､毎回同じ画面が出る
  ([docs/operations.md](docs/operations.md) を参照)｡
- `--fixture ... --perf 60` で自分の RSS と CPU を 1 秒ごとに TSV で流し､
  要約を出して終了する (同じく docs/operations.md)｡

認証の手段は X でのサインインだけである｡app-only の bearer token は削除した:
ホームタイムラインを読めず､投稿・repost・引用・いいね・削除もできなかった｡
それはこのアプリがやることのほとんどである｡

## ドキュメント

| ファイル | 内容 |
| --- | --- |
| [docs/timeline.md](docs/timeline.md) | ウィンドウが何を表示するか､1 行がどう組み立てられるか |
| [docs/writing.md](docs/writing.md) | 投稿､返信､引用､repost､いいね､削除､ブラウザで開く |
| [docs/media.md](docs/media.md) | 添付画像と投稿者のアバター |
| [docs/operations.md](docs/operations.md) | ログ､コードメトリクス､テスト |
| [.claude/skills/app-bundle](.claude/skills/app-bundle/SKILL.md) | `.app` バンドルのビルド (release と development) |

## 必要なもの

`macos-blade` フィーチャを有効にしてあるので､ビルドに `xcrun metal` は要らない｡
これは Command Line Tools ではなく完全な Xcode に同梱されるものである｡描画は
代わりに blade を通る｡


## セットアップ

OAuth client id が必要である｡ほかに認証情報は無い (#33)｡

**推奨: `oauth_client_id` を `config.toml` に置く｡** twigpui が持つ唯一の
非秘匿な認証情報であり (public な OAuth client には client secret が無い)､
dotfiles リポジトリにコミットして構わない — そして export した環境変数と違い､
twigpui をどう起動しても読まれる｡export した `X_OAUTH_CLIENT_ID` は､別の
ターミナル､プロファイルを一度も source していない新しいシェル､
Finder/Spotlight/Dock から起動した `.app` (#40) からは見えない — どれもシェルの
環境を継承しないためである｡この隙間がまさに #54 の原因だった: 保存済みの
セッションが期限切れになり､そのシェルには client id が無いのでリフレッシュ
できず､アプリは理由を画面に出さないまま､読み取り専用の劣化モードで黙って
動きつづけた｡

```sh
mkdir -p ~/.config/twigpui
cat >> ~/.config/twigpui/config.toml <<'EOF'
oauth_client_id = "…"
EOF
cargo run
```

(`~/.config/twigpui/config.toml` が既定のパスである — `$XDG_CONFIG_HOME` が
設定されていれば `$XDG_CONFIG_HOME/twigpui/config.toml`｡パス解決の全体と､
このファイルが受け付けるほかのキーは後述の "`config.toml`" を参照｡)

`config.toml` を使いたくなければ､ローカルに `.env` を置いてもよい｡`dotenvy` が
環境変数として読み込む:

```sh
cp .env.example .env
$EDITOR .env          # X_OAUTH_CLIENT_ID を書く
cargo run
```

<a id="migrating-from-the-bearer-token"></a>
### bearer token からの移行

`X_BEARER_TOKEN` は無くなった｡それを設定していたなら:

1. `X_OAUTH_CLIENT_ID` を設定するか､`config.toml` に
   `oauth_client_id = "…"` を足す — X Developer Portal で作った public な
   OAuth client の client id である｡秘密情報ではない｡
2. `config.toml` から `bearer_token` キーを消す｡残っている間は起動が失敗する｡
   意図的である: 無視すれば､何も読んでいないのに設定できているつもりのままに
   なる｡
3. twigpui を起動して **Sign in with X** を一度クリックする｡セッションは
   `$XDG_STATE_HOME/twigpui/oauth_tokens.json` に永続化され､以降の起動 —
   ブラウザを開かない `--fetch-only` と `--fetch-post` を含む — はそれを
   再利用する｡

app-only アクセスはホームタイムラインを読めず (401)､投稿・repost・引用・
いいね・削除もできなかった｡残すことは､設定解決・タイムラインのソース・
ヘッダーのあらゆる操作要素に 2 本目の認証情報の経路を通すことを意味し､
見返りは厳密に能力の劣るアプリだった｡

環境変数は常に `config.toml` の同名キーを上書きする (後述の "`config.toml`" を
参照) — 一度きりの上書きには便利だが､ターミナルや起動方法をまたいで､毎回
export し直すのを覚えていなくても効くようにしたいなら､`oauth_client_id` を
ファイルに置くことの代わりにはならない｡

| 変数 | 必須 | 既定値 | 意味 |
| --- | --- | --- | --- |
| `X_OAUTH_CLIENT_ID` | **はい** | — | "Sign in with X" 用の OAuth 2.0 client id — 非秘匿で､`config.toml` に `oauth_client_id` として置いてもよい |
| `X_TARGET_USERNAME` | いいえ | `XDevelopers` | `--fetch-only` が取得するスクリーンネーム｡先頭の `@` は付けない |
| `X_MAX_RESULTS` | いいえ | `20` | 1 回の取得あたりの投稿数､5–100 |
| `X_LIST_ID` | いいえ | 未設定 (development ビルドは自身のリストを既定にする, #169) | ホームタイムラインの**代わりに**ウィンドウへ表示する X List の数値 id — `config.toml` では `list_id` (#161) |
| `X_MIN_FETCH_INTERVAL_SECONDS` | いいえ | `60` | 取得を実行できる間隔の下限 (秒) (#10) |
| `X_THEME` | いいえ | `light` | カラーテーマ: `light`, `dark`, `system` (OS の外観に従う) — `config.toml` では `theme` (#19) |
| `X_REQUEST_PRICE` | いいえ | 未設定 | API リクエスト 1 回あたりの価格｡単位は任意 — `config.toml` では `request_price` (#18, [Usage tracking](.claude/skills/x-api-budget/reference/app-behavior.md#usage-tracking) を参照) |
| `X_DAILY_REQUEST_BUDGET` | いいえ | 未設定 | 1 日のリクエスト数の予算｡近づくとヘッダーの使用量の行が色付く — `config.toml` では `daily_request_budget` (#18) |
| `X_AUTO_SYNC_LIST` | いいえ | `true` | アプリの実行中､`X_LIST_ID` のメンバーをフォローに追従させつづける — `config.toml` では `auto_sync_list`｡**タイマーで課金する**; 後述 |
| `X_SYNC_INTERVAL_SECONDS` | いいえ | `21600` (6 時間) | バックグラウンド同期が diff の間に待つ時間｡`900` 未満の値は拒否する — `config.toml` では `sync_interval_seconds` |
| `X_SYNC_PRUNE_LIMIT_PERCENT` | いいえ | `10` | バックグラウンド同期が 1 回の diff で削除できるメンバーの上限 (パーセント)｡超えた分の削除は保留し､`--sync-list --apply --prune` での確認に回す; `100` で上限を外す — `config.toml` では `sync_prune_limit_percent` (#176) |
| `X_SYNC_WRITES_PER_BATCH` | いいえ | `2` | 追いつき処理の間にバックグラウンド同期が 1 バッチで送るリスト書き込みの数 (#197)｡`1`–`20`; 上限は X が文書化した書き込みウィンドウ (15 分あたり 300) を 1 分へならした値である｡バッチの間隔は 90〜300 秒に揺らぐので､持続レートはおよそ「この値 ÷ 195 秒」になる｡既定値での実行が拒否を出さないと分かってから上げる — `config.toml` では `sync_writes_per_batch` |
| `X_AUTO_REFRESH` | いいえ | `true` | ウィンドウが開いている間､新しい投稿をタイムラインにポーリングする — `config.toml` では `auto_refresh` (#21)｡`false` なら､アプリはクリックしていないものを一切送らない |
| `X_AUTO_REFRESH_INTERVAL_SECONDS` | いいえ | `180` (3 分) | 自動更新がポーリングの間に待つ時間｡`X_MIN_FETCH_INTERVAL_SECONDS` を下回る値は拒否する — `config.toml` では `auto_refresh_interval_seconds` |
| `X_FOLLOW_NEW_POSTS` | いいえ | `true` | 先頭にいるとき､ポーリングで届いた新しい投稿がひとりでに画面へ流れ込むようにする (#22) — `config.toml` では `follow_new_posts`｡表示だけの話で､何をいつ取得するかは変えない｡実行中は View → Follow New Posts (`⌘⇧F`) で切り替える |

`.env` は gitignore してある｡認証情報をコミットしないこと｡

### 自動更新

ウィンドウは 3 分ごとにタイムラインをポーリングする｡届いた新しい投稿がその後
どうなるかは､いまどこにいるかで決まる (#22):

- **先頭にいるとき** — そのまま流れ込み､滑るように視界へ降りてくる｡開きっぱなし
  のウィンドウはひとりでに動きつづける｡View → Follow New Posts (`⌘⇧F`) で
  切り替える; ホイールに触れた瞬間に滑りは止まる｡
- **下の方を読んでいるとき** (または follow が off のとき) — リストの下端に
  浮かぶ **"↑ N new posts"** の toast の裏で待つ｡押すまで何も動かない —
  リストも､スクロール位置も｡押す (または `⌘⇧R`､View → Show New Posts) と
  表示され､先頭へ跳ぶ｡toast はスクロールしても下端に留まり､流れ込みの間は
  まだ視界の上にある件数を数え下げて､0 で消える (#206)｡

後半が意図した設計である｡頼んでいない取得が､読んでいる本文を画面の下へ滑らせて
はならない｡だからポーリングは読書中のタイムラインに触れず､バーが差し出す
バッファを埋めるだけである｡課金なしでこの流れを見るには､
`cargo run -- --fixture fixtures/timeline.json` が 5 秒後にフィクスチャの
保留分を届ける｡

**コストは 5 分タイマーという響きほど大きくない｡** 読み取りは返ってきた投稿ごと
に課金され､UTC の 1 日の中で重複が除かれる｡だから 1 日ポーリングしても､その日に
本当に新しかった投稿の分だけが課金される — どう届こうと､それを読むコストは同じ
である｡繰り返し課金されるのは UTC の深夜を越えた最初の先頭ページだけで､
`X_MAX_RESULTS` が上限になる｡

`⌘R` と `⌘⇧R` は金の面で意図的に正反対である: `⌘R` は取得を買い､`⌘⇧R` は
タイマーが既に払ったものを見せる｡

`X_AUTO_REFRESH=false` で止める｡このスイッチの裏でタイマーが回りつづけることは
無い — ループはそもそも開始されない｡

### バックグラウンドのリスト同期

`X_LIST_ID` を設定すると､ウィンドウは開いている間ずっと､そのリストのメンバーを
フォロー中のアカウントに追従させつづける｡フォローしたアカウントは追加され､
フォローを外したアカウントは削除される｡既定で有効で､`X_AUTO_SYNC_LIST=false`
で無効になる｡

**手でリストに追加したアカウントも削除する｡** リストは*それ自体が*ミラーであり､
それが契約のすべてである｡自分で選んで作るリストが欲しいなら､同期を切るか
`X_LIST_ID` を別のリストに向ける｡

**ステータスバーが何をしているかを示す** (#174)｡"List sync: up to date" が定常
状態､"List sync: 1100 to go" は計画を消化中の追いつき処理である｡"List sync:
no list configured" と "List sync: re-authorize to enable" は､止まっている
同期が何に阻まれているかを示す｡これが入るまでこの機能はウィンドウから見えず､
あと数時間かかる追いつき処理は､何も起きていない状態とまったく同じに見えた｡

**クリックすると同期が始まる**｡開始できる状態ならどれでも始まる｡先に確認を出す｡
それが買う読み取りは､このアプリで最も高価なクリックだからである｡この方法で
始めた同期は間隔を無視する — それがボタンの意義である — が､レートリミットと
未消化の計画は無視しない: 追いつき処理が途中まで進んでいれば､両側を再び diff
する代金を払わずにその計画を再開する｡

`X_AUTO_SYNC_LIST=false` ではタイマーが回らず､同期はこのボタンでしか起きない｡
その実行はやることが無くなった時点で止まる｡

**タイマーで金を使う｡** diff のたびにフォローリスト全体とリストのメンバー全体を
読み､どちらも返ってきたアカウントごとに課金される｡1 日 4 回の diff なら､X が
文書化した 24 時間の重複排除がこれらの読み取りに効くならフォロー 1000 件あたり
約 $2､効かないなら約 $8 である — `x-api-budget` がその規則を実測できているのは
Posts だけである｡だから間隔の既定は 6 時間で､15 分未満は拒否する｡

書き込みはまとめて送らずに分散させる: 既定で 1 バッチ 2 件
(`sync_writes_per_batch`)､バッチの間は 90〜300 秒､バッチ内の書き込みどうしは
3〜20 秒｡どちらの間隔も毎回引き直し､伸びる側にしか振れない｡持続的には
およそ 0.6 件/分なので､数千アカウント遅れているリストは追いつくのに数日かかる｡
ステータスバーから手で押した同期はこの間隔を無視する｡

X は `x-rate-limit-*` ヘッダーが説明しない上限でリストへの追加を拒否する
(#193, #197)｡拒否は追いつき処理を失敗させずに一時停止させ､連続した拒否ほど
長く待つ (15 分から 6 時間まで)｡ステータスバーは最初の拒否で "rate limited"､
2 回目からは赤字で "refused N× in a row" と出す｡ログには拒否 1 件につき 1 行､
429 のヘッダーとボディが残る｡

進捗は変更 1 件ごとに `$XDG_STATE_HOME/twigpui/sync_plan.json` へ書かれるので､
追いつき処理の途中で終了しても何も失わない — 次の起動が止まった場所から
ちょうど再開する｡`$XDG_STATE_HOME/twigpui/sync_state.json` は最後に diff を
実行した時刻を持つ｡これが再起動時に両方の読み取りを再び払うのを止める｡加えて
バックオフ — いつまで書き込みを止めるか､連続何回拒否されたか — も持つので､
再起動が上限へ送り込むこともない｡バッチとバッチの間もここに書かれるので､
追いつき処理の途中で再起動しても即座に送り直さない｡

必要なスコープは `--sync-list` と同じである｡それより前に取ったセッションは､
画面へのエラーではなくログの 1 行を残してスキップされる — "Re-authorize" を
一度クリックすれば､再起動なしで同期が始まる｡

間隔は起動時点ではなく最後の diff から数え､その記録はディスクに残る｡アプリを
再起動しても同期は始まらない: 最後の実行が 1 時間前なら､次はまだ 5 時間先である｡

debug ビルドも同期するが､上のコストはどれも当てはまらない (#169): フォロー
グラフを読まず､固定のシード screen name を development 用のリストへミラーし､
独自の `twigpui-dev` state ディレクトリを使う｡この節の数字は `--release`
ビルドが使う額である｡

### `--sync-list` — フォローをリストへミラーする (#163)

List は中にあるものしか表示しないので､#161 のウィンドウの出来はリストのメンバー
次第である｡`--sync-list` はフォロー中のアカウントとリストのメンバーを diff し､
一方をもう一方へミラーする｡

```sh
cargo run --release -- --sync-list          # dry run: 両側を読み､計画を書き､表示する
cargo run --release -- --sync-list --apply  # 追加を送る
cargo run --release -- --sync-list --apply --prune   # …と削除も
```

**ここでは `--release` が効いている** (#169)｡debug ビルドは development
プロファイルであり､あなたのフォローからあなたのリストへではなく､固定の 4 つの
X アカウントから*development 用の*リストへ同期する｡`--release` を落としても
失敗はしない — 黙って別の組み合わせを同期する｡それがまさに development
プロファイルの目的である｡[development ビルド](#development-builds) を参照｡

**dry run は無料ではない｡** どちらの読み取りも返ってきたアカウントごとに課金
されるので､数千件のフォローに対して 1 回走らせればセントではなくドル単位になる｡
最初の `--apply` の前に developer console で価格を確認すること — このアプリ自身の
使用量の数字はリソースではなくリクエストを数えているので､#162 は開いたままである｡

計画は `$XDG_STATE_HOME/twigpui/sync_plan.json` に書かれ､各エントリは反映される
たびに印が付く｡だから中断した `--apply` は､どちらの側も読み直す代金を払わずに
ファイルから再開する｡ファイルに計画が無い `--apply` はエラーである: 計画を作る
のは dry run である｡

**ここでは**削除に `--prune` が要る｡CLI ではリストが手で追加したアカウントを
抱えている可能性があり､それを消すかどうかは利用者の判断のままにする｡上の
バックグラウンド同期は無条件に削除する｡増えるだけのミラーはミラーではない
からである｡

どちらも同じ計画ファイルを読むので､dry run が作った計画は次にバックグラウンド
同期が消化するものになる — その削除も含めて｡何も適用されない状態で diff を
眺めたいなら､先に同期を切ること｡

バックオフも共有する｡バックグラウンド同期が拒否を受けてバックオフしている最中の
`--apply` (#197) は､その旨を stderr に出して**それでも送る** — 意図した 1
バッチは､上限が解除されたかを知る最も安い方法である — そして返ってきた結果は
バックグラウンド同期のためにも記録される: 通った書き込みは連続を終わらせ､拒否は
連続を伸ばす｡

どちらの側も､#163 より前のこのアプリが要求していなかったスコープ
(`follows.read`, `list.write`) を必要とする｡だから既存のセッションは何も使う前に
拒否される — twigpui を起動して "Re-authorize" を一度クリックすること｡

## X でのサインイン (OAuth 2.0 Authorization Code + PKCE)

このアプリがやること — ホームタイムライン､投稿､返信､いいね､削除 — はすべて
公開データの読み取りではなく*本人として*の操作なので､どれも user context の
OAuth 2.0 セッションを必要とする｡twigpui は Authorization Code + PKCE フロー
(RFC 6749 + RFC 7636) でそれを取得する｡その全体を "Sign in with X" ボタンから
ウィンドウ内で実行する｡

**Developer Portal 側の前提｡** X Developer Portal で **public client**
(client secret 無し) を登録し､この redirect URI をそのまま追加する:

```
http://127.0.0.1:8733/callback
```

X は完全一致を要求するので､ポートを動的にはできない — `8733` はコード内
(`profile::Profile::loopback_port`) に固定してあり､Portal の登録と一字一句
一致していなければならない｡development ビルドは `8734` と専用の X app を使う｡
[development ビルド](#development-builds) を参照｡

**Client id｡** Portal が表示する client id を `X_OAUTH_CLIENT_ID` (環境変数
または `.env`) か `config.toml` の `oauth_client_id` へ写す｡非秘匿である —
public client には守るべき secret が無い — ので dotfiles リポジトリに
コミットして構わない｡

**スコープ｡** twigpui は `tweet.read users.read tweet.write like.write
offline.access` を要求する: 投稿の読み取り､user context の解決､投稿 (#14)､
いいね (#68)､再認証を求めないセッションのリフレッシュに足りる分である｡

**#14 や #68 より前にサインインしていたなら､** 保存済みのセッションは
`tweet.write` や `like.write` より古く､まだ投稿もいいねもできない — API は
書き込みを 403 で拒否し､アプリ側にそれを自力で直す手立ては無い｡twigpui は
セッションごとに付与されたスコープを記録し (記録が始まる前のセッションは
"unknown" として扱い､「たぶん大丈夫」とは決して見なさない)､現在のセッションに
アプリが必要とする書き込みスコープが欠けているときは､通常のリロード/サインイン
の操作の隣にヘッダーが **"Re-authorize"** ボタンを出す｡クリックすると上の
サインインフローを最初から最後までもう一度走らせる — 新しいブラウザの同意画面､
新しいトークン､すべてのスコープを一度に — そしてそれ以外にアプリで変わるものは
無い｡

**"Sign in with X" をクリックすると何が起きるか:** アプリは既定のブラウザで X の
同意画面を開き､`127.0.0.1:8733` (development ビルドでは `8734`) の短命な HTTP
リスナーが戻りのリダイレクトを受け取る (最大 2 分待つ)｡2 つのポートは衝突しない
ので､片方のビルドのサインインともう片方のサインインを同時に進行させられる｡X が
authorization code を付けてリダイレクトすると､twigpui はそれを access token と
refresh token に交換し､そのまま通常のリロードへ入る｡

**トークンの保存先｡** `$XDG_STATE_HOME/twigpui/oauth_tokens.json` (後述の表を
参照)｡`0600` (所有者の読み書きのみ) で書き､access token・refresh token・絶対
時刻の有効期限を素の JSON で持つ｡これは開発用途専用の単一ユーザー向けアプリ
である｡Keychain を使わなかった理由は issue #7 を参照 (ad-hoc ビルドはリビルドの
たびに署名 identity が変わり､その都度 Keychain アクセスの確認が出ることになる)｡

保存済みのセッションは､アプリがトークンを必要とするたび､期限の少し前に自動で
リフレッシュされる — refresh token が有効なかぎり再認証は求められない｡

<a id="file-locations-xdg-base-directory"></a>
### ファイルの置き場所 (XDG Base Directory)

twigpui が永続化するものはすべて 3 つのディレクトリの下にある｡
[XDG Base Directory
spec](https://specifications.freedesktop.org/basedir-spec/latest/) に従って
解決し､起動時に (mode `0700` で) 作成する:

| 変数 | 既定値 | 持つもの |
| --- | --- | --- |
| `XDG_CONFIG_HOME` | `~/.config/twigpui/` | `config.toml` |
| `XDG_CACHE_HOME` | `~/.cache/twigpui/` | レスポンスキャッシュ: `user_ids.json`, `timeline-<user_id>.json` (#9), `me.json`, `home-timeline-<user_id>.json` (#11), `thread-<reply_id>.json` (#12), `avatars/` (#64), `media/` (#65) |
| `XDG_STATE_HOME` | `~/.local/state/twigpui/` | `oauth_tokens.json` (mode `0600`), `rate_limit.json` (#10), `usage.json` (#18), `reposted_posts.json` (#15), `liked_posts.json` (#68), `logs/` (#49) |

`XDG_*` 変数は､空でない絶対パスが設定されているときにだけ尊重される｡相対パスや
空の値は spec に従って既定値へ落ちる｡

development ビルドは 3 つとも `twigpui` ではなく `twigpui-dev` を付けるので､
インストール済みのアプリとファイルを共有しない —
[development ビルド](#development-builds) を参照｡

<a id="development-builds"></a>
### development ビルド

debug ビルド (`cargo run` または `./scripts/build-app-bundle.sh --dev`) は､
`.app` バンドルが配る release ビルドとは別のインストールである｡別の X app に
サインインし､独自のセッションとキャッシュを持ち､それをウィンドウタイトルに
示す (#169):

| | release ビルド | debug ビルド |
| --- | --- | --- |
| ディレクトリ | `~/.config/twigpui/` など | `~/.config/twigpui-dev/` など |
| OAuth redirect URI | `http://127.0.0.1:8733/callback` | `http://127.0.0.1:8734/callback` |
| ウィンドウタイトル | `twigpui` | `twigpui (dev)` |
| バンドル | `dist/twigpui.app` | `dist/twigpui-dev.app` ([`app-bundle` skill](.claude/skills/app-bundle/SKILL.md) を参照) |
| バンドル id | `com.github.usadamasa.twigpui` | `com.github.usadamasa.twigpui.dev` |
| アイコン | `assets/AppIcon.png` | 同じ図柄の彩度を落としたもの |
| 既定の `list_id` | 無し — ホームタイムライン | 使い捨てのリスト (`profile.rs` にある) |
| `--sync-list` のソース | フォローしている全員 | 固定の 4 つの X アカウント |

最後の 2 行が､このアプリの金のかかる部分を触るコストを抑えている｡development の
`--sync-list` が本物のフォローグラフを読めば､dry run はその全アカウント分を
課金される (#163)｡本物の List を既定にすれば､忘れられた export の上から書き換え
かねない — だから development ビルドは自分用の list id と自分用の 4 アカウントの
ソースを持ち､どちらも `src/profile.rs` にある｡`X_LIST_ID` と `list_id` は今も
既定値を上書きする｡同期のソースに上書き手段は無い｡development ビルドが本物の
読み取りコストを使うことこそが､防ぎたい当のものだからである｡

どちらになるかはコンパイル時に `debug_assertions` で決まり､フラグも環境変数も
無い｡意図的である: これが防ぐ失敗は*忘れること*であり､debug バイナリを説き伏せて
release インストール側のトークンやキャッシュを触らせることはできない｡代償は両者が
食い違う唯一のケースである — **このチェックアウトからの `cargo run --release` は
release プロファイルを使う**｡つまりインストール済みアプリのファイルを使う｡
最適化された見た目の development アプリが欲しいときは
`./scripts/build-app-bundle.sh --dev` を使う｡まさにこの理由から､意図的に debug
でビルドする｡

**用意のしかた｡** X Developer Portal で 2 つ目の public client を
`http://127.0.0.1:8734/callback` を redirect URI として登録し､その client id を
development ビルドだけが読む場所へ置く:

```sh
mkdir -p ~/.config/twigpui-dev
cat >> ~/.config/twigpui-dev/config.toml <<'EOF'
oauth_client_id = "…development 用アプリの client id…"
EOF
```

`.env` ではなく `config.toml` にする理由: このチェックアウトの `.env` はここから
走るどのプロファイルからも読まれるので､そこに置いた client id は release
プロファイルの実行についていき､インストール済みアプリの state に届いてしまう｡

### `config.toml`

`$XDG_CONFIG_HOME/twigpui/config.toml` は任意の､手で編集する設定ファイルである｡
キーは上の環境変数と同じである:

```toml
target_username = "XDevelopers"
max_results = 20
list_id = "2091351590695588200"
min_fetch_interval_seconds = 60
oauth_client_id = "…"
theme = "light"
request_price = 0.02
daily_request_budget = 500
auto_sync_list = true
sync_interval_seconds = 21600
sync_prune_limit_percent = 10
sync_writes_per_batch = 2
auto_refresh = true
auto_refresh_interval_seconds = 300
```

ファイルが無くても構わない — ファイルレベルの設定が無いというだけである｡
優先順位は **環境変数 > `config.toml` > 組み込みの既定値** で､環境変数は常に
ファイルに勝つ｡`oauth_client_id` はコミットして安全である — public な
client id であり､秘密情報ではない｡

`list_id` は x.com のリスト自身の URL に含まれる数字である
(`https://x.com/i/lists/<list_id>`)｡設定すると 2 つ目のビューが増えるのではなく､
ホームタイムラインが置き換わる: `GET /2/users/:id/timelines/reverse_chronological`
はこのアカウントに対してフォロー中の投稿者の投稿を返さなくなり (#157)､ここから
それを直す手立ては無い｡だからフォローの形をしたフィードを読む唯一の方法が List
である｡数字でない値は無視されずに起動を失敗させる — 黙って空のホームタイムライン
へ落ちると､リストが空だったように見えるからである｡

`bearer_token` キーは無視ではなく**拒否**する (#33): それが指していた認証情報は
もう存在せず､それを抱えたままのファイルは､何も読んでいないのに設定済みに
見えてしまう｡

### テーマ

`theme` は `light`, `dark`, `system` を受け付ける (大文字小文字は区別せず､前後の
空白は落とす)｡`X_THEME` または `config.toml` の `theme` で指定し､優先順位は上の
ほかと同じく 環境変数 > ファイル > 既定値 である｡既定値は `light`｡`system` は
OS の外観に従い､gpui の `Window::appearance()` で起動時に一度だけ読む｡認識できない
値は起動エラーにしない — テーマの打ち間違いは見た目の話で､アプリを止めるほどでは
ない — `light` へ落ち､無視した値を挙げた警告を stderr に出す｡


## キーボードショートカット

| キー | 動作 |
| --- | --- |
| `⌘R` | リロード |
| `⌘⇧R` | 自動更新が既に取得済みの投稿を表示する (課金なし) |
| `⌘N` | コンポーザーへフォーカスする |
| `esc` | コンポーザーから抜ける (下書きは保持する) |
| `⌘↑` | 最新の投稿へ戻る |
| `⌘Q` | 終了する |
| `⌘W` | ウィンドウを閉じる |
| `⌘M` | 最小化する |

どれもメニューバーにある (#99)｡macOS のユーザーがキー操作を探す場所だから
である｡#58 は､誰も開かないヘルプ画面は何も説明しないという理由で､最初の 4 つを
ヘッダーの下の常設の帯にも出していた｡#95 はその帯を消した｡ツールバーの下に
ヒントの行を並べるのはネイティブアプリのやることではなく､メニューバーがそれを
不要にしていたからである｡

**どのバインドも裸の印字可能キーではない｡意図的である｡** #58 が本当に問題に
している危険は､投稿を入力している最中に裸の `j`/`k`/`n` が発火することである｡
今あるバインドにそれはできない｡どれも `⌘` を伴うか､何も入力しない名前付きキー
(`esc`) だからである｡裸のキーが価値を持つのは投稿を選択できるようになってからで､
それにはコンポーザーのフォーカスが外す 2 つ目の key context が要る —
`menu::KEY_CONTEXT` を参照｡

**`⌘Q` は key context を持たない唯一のバインドである｡** ほかはタイムラインに
ついての問いに答えるもので､答えるビューに属する｡終了はウィンドウの仕事ではなく､
スコープを付ければフォーカスが別の場所にある間ずっと `⌘Q` が何もしないことに
なる — だからグローバルに登録し､ウィンドウのルートではなく `App` 側で処理する
(#99)｡

**投稿するキーは無い｡** コンポーザーのボタンが唯一の手段であり､実際そう使われて
いた｡`⌘↩` は #58 から割り当てられていたが､#142 がそれとメニュー項目の両方を
消した｡素の `↩` は一度も割り当てていないし､今もしていない｡削除より長生きする
理由がある — 改行を入れつづけなければならないし､投稿は取り消せない｡

**`esc` はフォーカスを移すだけである｡** 下書きは打ったそのままにしておく: 誤打で
失えば取り返せないし､下書きを決して失わないことがコンポーザーの主な約束である
(#14)｡

**リロードは読み手を動かさない｡** 途中まで下っているリストの先頭に投稿が届けば､
そのままでは目の下ですべてが滑ってしまう｡タイムラインは届いた投稿の数だけ
スクロールするので､読んでいた行はその場に留まる｡いちばん上にいるときは何も
動かず､新しい投稿がただ現れる (#22)｡

**リロードは何をしたかを伝える｡** ヘッダーの下の控えめな行が､届いた投稿の数を
報告する — 0 件のときも含む｡それは前後で画面がまったく同じになり､押しても何も
起きなかったように見えるケースだからである｡数えるのはスクロールが打ち消すのと
同じ投稿なので､数字と動きは常に一致する (#141)｡

**"Load older" にショートカットは無い｡** 押すたびに課金されるリクエスト 1 回で
過去へ遡るので､誤打で金を使うキーは便利さではない｡`⌘R` もリクエストを使うが､
どのアプリにもあるリロードの操作で､誤って押すものではない — しかもボタンと同じ
スロットル (#10) とクールダウンの報告 (#57) を通るので､`⌘R` を押しっぱなしに
しても､このアプリがループで課金するのを止めるための間隔を追い越せない｡


## メニューバー

| メニュー | 項目 |
| --- | --- |
| twigpui | About twigpui, Quit twigpui |
| File | New Post |
| View | Reload, Back to Top |
| Window | Minimize, Close Window |

どの項目もキー操作と同じアクションを発行し､macOS はキーマップからキー等価表示を
その横に描く｡1 つの `menu::Shortcut` 定数がキー操作､2 通りの文言､**そして
アクション**を持ち (#119)､キーバインド､画面上の帯､メニュー項目はすべてそこから
組み立てられる — だから 4 つのうち 1 つだけを変えて､ほかを変えないままにはできない｡

この主張はかつてコードが保証する範囲より広かった｡#119 まで定数はキーとラベルしか
持たず､どのアクションを発行するかは `init` と `menus` の両方に書き直されていて､
ラベルを誤ったアクション型と組み合わせても普通にコンパイルが通った｡テストが
捕まえるべき範囲は今は狭い: どのメニューに項目が属するかは今も `menus` が決めるので､
ショートカットがメニュー用のラベルを持ちながら､どのメニューにも入らないことは
ありうる｡

2 つの一覧で文言が違うのは意図的である: メニュー項目は単独で読まれ ("New Post")､
帯は見出しの下のヒントの並びとして読まれる ("⌘N Focus the composer")｡

**Window メニューの名前は効いている｡** gpui がメニューを AppKit の
`setWindowsMenu_` へ渡すのは､名前がちょうど `Window` のときだけである｡改名しても
`⌘W`/`⌘M` は動きつづける — ただのバインドだからである — が､そのメニューは macOS
がウィンドウ一覧として扱うものではなくなる (#109)｡

**`⌘W` はアプリを終わらせる**｡ウィンドウが 1 つだからだが､それは最後のウィンドウ
を閉じたときに明示的に終了しているからにすぎない (#139)｡gpui は放っておくと
プロセスを生かしつづける｡もう 1 枚ウィンドウを開けるアプリならそれが正しい｡この
アプリはできないので､`⌘W` はかつて､画面に何も出ておらず `⌘Q` でしか届かない
プロセスを残していた｡

`⌘Q` と同じく確認は出さず､送っていない下書きも一緒に消える — `⌘Q` がずっと
抱えてきたのと同じ危険である｡

