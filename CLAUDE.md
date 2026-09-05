# CLAUDE.md — twigpui

Rust + [gpui](https://crates.io/crates/gpui) で書いた X (Twitter) タイムラインビューア。
macOS 専用、開発用途のみ。他プラットフォームは考慮しない。

## バックログ運用ルール

#4 の指示に基づく。この節のルールは変更しない。

- バックログに書かれたタスクは分解して個別の issue にする。
- issue にはそれぞれラベルを振る。ラベル体系と、立てる前の重複チェックは
  `backlog-triage` スキルに従う。
- issue 化したら、バックログ issue (#4) の一覧からは削除してよい。
- 優先度は適宜判断する。プロジェクトで管理してもよい。
- #4 の「指示」セクションは更新してはならない。

## 開発ポリシー

### 開発体験の改善は優先度キューに並べず、随時やる

ビルド時間、Linter の厳格化、コードスメルの自動検知、ログ出力のような
開発体験・保守性の課題は、優先度ラベルの順番を待たせない。
機能追加の作業中に手が届いたらその場で拾う。

後回しにすると効き目が落ちる性質のものだから。Linter が緩いままコードが増えれば
後から厳しくする手間が増える。ログが無いまま調査を重ねれば、同じ調査を何度も
やることになる。

ただし機能追加の PR に混ぜない。別コミット・別 PR に分ける。

### 当たり前の機能を優先する

X クライアントとして当然あるべき操作 (読む、書く、反応する、外部で開く) を、
体験の改善や別フロントエンドより前に置く。backlog (#4) は読み取り中心の計画だったので、
いいね・画像表示・外部への導線といった基本機能が漏れている。
**「issue が無い」は「やらなくてよい」ではない。**

## ビルドとテスト

push 前にこの 3 つを通す。CI (`macos-latest`) が見るのと同じもの。

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`cargo run -- --fetch-only` でウィンドウを開かずに取得結果を標準出力へ流せる。

テストが large (子プロセス・`thread::sleep`・ネットワーク) になると
`scripts/test-sizes.sh --check` が CI で落ちる。サイズの定義と allowlist
(理由つき。`thread::sleep` とネットワークは置けない) は同じスクリプトの
先頭にあり、今日の数字は `docs/test-sizes.md` にある。

## 実装上の制約

- **`macos-blade` 必須**: gpui の既定ビルドは `xcrun metal` を要求し、これは Command Line
  Tools には含まれない。`Cargo.toml` は `default-features = false` + `macos-blade` で
  ビルドしているので、この指定を外さない。同時に x11 / wayland も落ちるため Linux 向けには
  ビルドできない (CI が macOS ランナーのみなのはこのため)。
- **`cargo` は sandbox の外で動かす**: `.claude/settings.json` の
  `sandbox.excludedCommands` に `cargo *` を置いている。この指定を外すと `cargo fetch` が
  crate の展開途中で止まり、`cargo run` はウィンドウを開けない。sandbox 由来の失敗を
  切り分けるときは `sandbox-troubleshooting` スキルに従う。
- **中間成果物は `target/` に無い**: `.cargo/config.toml` の `build.build-dir` が
  `deps/` `build/` `incremental/` を `$CARGO_HOME/build/twigpui` へ逃がしている。
  worktree をまたいで依存を再利用するためで、`target/{profile}/` に残るのは
  最終実行ファイルだけになる。中間成果物のパスを踏むスクリプトを書くときは
  `target/` を前提にしない。CI と `scripts/coverage.sh` は
  `CARGO_BUILD_BUILD_DIR` でこれを上書きしていて、理由は
  `.cargo/config.toml` のコメントにある。共有 build-dir は自動で掃除されないので、
  溜まったらまとめて消す。
- **lint はすべて `deny`**: `unsafe_code` は forbid。`warn` に下げると手元で通って CI だけが
  落ちるので、**レベルを下げて回避しない**。バイナリクレートなので公開項目は `pub(crate)`。
  clippy に弾かれたときの書き直し方は `rust-lint-gauntlet` スキルに従う。
- **ファイルサイズは CI が落とす**: `scripts/code-metrics.sh --check` が
  実装行数を一律の上限 (600 行) と突き合わせる。超えたら分割する。
  上限より大きいまま残っているファイルの天井は `metrics-baseline.tsv` にあり、
  上げられない (#241)。落ちたときの手順は `code-metrics-ratchet` スキルに従う。
- **カバレッジは報告のみ、閾値は無い**: `scripts/coverage.sh` が実装だけの
  カバレッジを出す (テストは同じファイルの `#[cfg(test)]` にあるので、素の
  `cargo llvm-cov` の数字は 25 ポイント以上ふくらむ)。CI の Coverage ジョブは
  必須チェックではない。穴の読み方と、埋める価値のある穴の選び方は
  `coverage-gaps` スキルに従う。**数字を上げるためのテストは書かない。**
- **行の中にも post の面がある**: repost・引用・スレッドの行は、`quote_card` と
  `thread_row` という埋め込まれた post の面を本文の中に持つ。ウィンドウ全体へ
  効かせるつもりの見た目 (背景の不透明度、テーマの色、角丸、余白) は、枠だけに
  渡すとここが取り残される (#267 の透過がそうだった)。RT・引用・返信の行を
  触るとき、そういう見た目を足すときは `repost-rendering` スキルの点検表に従う。
- **課金に直結する API**: X API はプリペイド残高からの従量課金。呼び出しを足す・
  取得件数 (`max_results`) を変える・頻度を変える・キャッシュを触るときは
  `x-api-budget` スキルに従う。
- **公開 spec に従うと壊れる**: X API は post 語彙と tweet 語彙が並存していて、
  リクエストで使った綴りがレスポンスの綴りを決める。docs.x.com の `openapi.json` は
  post 語彙しか宣言していないが、このアプリは tweet 語彙 (`tweet.fields`,
  `includes.tweets`) で書かれている。`post.fields` へ寄せると `#[serde(default)]` の
  せいでエラーが出ないまま空になる。エンドポイントを選ぶ・パラメータを足す・4xx を読む
  ときは `x-api-endpoints` スキルに従う。
- **テストはネットワークを叩かない**: パースとエラー変換をフィクスチャ JSON で検証する。
  テストが課金を発生させないことを保ちつづける。
- **寸法は assert できる。色と字面は撮って見る**: テストプラットフォームでも
  レイアウトは走る (#184 で実測)。要素に `render::Addressable` で名前を付ければ
  `debug_bounds` が実際の bounds を返すので、間隔・位置・重なりはテストで押さえられる。
  `VisualTestContext::simulate_click` は hit test も通るため、クリックの経路も書ける。
  一方で色・フォント・字の詰まり方は `Scene` から先の話で、テストからは見えない。
  そちらは `cargo run -- --fixture` で課金なしの決定的な画面を出して撮る。
  手順は `fixture-visual-check` スキルに従う。
- **dev と本番は別インストール**: debug ビルド (`cargo run`) は `twigpui-dev` の
  XDG ディレクトリと callback port 8734、release ビルドは `twigpui` と 8733 を使う
  (`src/profile.rs`)。ディレクトリ名・ポート・ウィンドウタイトルを足したり変えたり
  するときは、両プロファイルで衝突しないことをテストで押さえる。
  切り替えは `debug_assertions` のみで、フラグも環境変数も無い。
  課金する既定値をプロファイルで分けるときも `src/profile.rs` に置く
  (list_id の既定と `--sync-list` の同期元がそう)。本番のデータを触る操作は
  `cargo run --release` か `.app` から実行する。`.app` の組み立ては
  `app-bundle` スキルに従う。
- **`.env` は編集不可**: パーミッション設定により Claude セッションからは `.env` を
  読み書きできない。認証情報が要るときは環境変数を export してもらう。
  `.env` は cwd で効くのでプロファイルを跨ぐ。dev 専用の設定は
  `~/.config/twigpui-dev/config.toml` に置く。
- **gpui は fork の patch 版**: Cargo.toml の `[patch.crates-io]` が
  `usadamasa/zed` の branch `gpui-0.2.2-draw-while-occluded` (0.2.2 を publish した
  commit + 2 commit) を `rev` で指す。差分は 2 機能。`gpui::set_draw_while_occluded` は
  `--fixture` の window を画面ロック中でも描き続けさせる (upstream はロック中に開いた
  window を 1 フレームも描かず、window capture が真っ黒になる。upstream への報告は
  zed-industries/zed#63217)。off が既定で本番は upstream と同一挙動。
  `Window::set_floating` は開いた後に window を floating level へ出し入れする
  (Window メニューの Float on Top、#267。upstream の `WindowKind` は開くときに決めるもので、
  macOS では `Floating` も `Normal` と同じ level に置かれる)。gpui を上げるときは
  その版を publish した commit の上に同じ patch を載せ直して `rev` を差し替える。
  git 依存なので初回ビルドは zed の repo を丸ごと clone する (時間がかかる)。
