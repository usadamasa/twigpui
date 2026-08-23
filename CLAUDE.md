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

## 実装上の制約

- **`macos-blade` 必須**: gpui の既定ビルドは `xcrun metal` を要求し、これは Command Line
  Tools には含まれない。`Cargo.toml` は `default-features = false` + `macos-blade` で
  ビルドしているので、この指定を外さない。同時に x11 / wayland も落ちるため Linux 向けには
  ビルドできない (CI が macOS ランナーのみなのはこのため)。
- **`cargo` は sandbox の外で動かす**: `.claude/settings.json` の
  `sandbox.excludedCommands` に `cargo *` を置いている。この指定を外すと `cargo fetch` が
  crate の展開途中で止まり、`cargo run` はウィンドウを開けない。sandbox 由来の失敗を
  切り分けるときは `sandbox-troubleshooting` スキルに従う。
- **lint はすべて `deny`**: `unsafe_code` は forbid。`warn` に下げると手元で通って CI だけが
  落ちるので、**レベルを下げて回避しない**。バイナリクレートなので公開項目は `pub(crate)`。
  clippy に弾かれたときの書き直し方は `rust-lint-gauntlet` スキルに従う。
- **ファイルサイズは CI が落とす**: `scripts/code-metrics.sh --check` が
  `metrics-baseline.tsv` の天井と突き合わせる。新規ファイルは先に登録しないと落ちる。
  天井の上げ下げは `code-metrics-ratchet` スキルに従う。
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
- **見た目は手元で見る**: gpui はテストプラットフォームでレイアウトを走らせないので、
  寸法も折り返しもコードから assert できない。`cargo run -- --fixture` で課金なしの
  決定的な画面を出して撮る。手順は `fixture-visual-check` スキルに従う。
- **dev と本番は別インストール**: debug ビルド (`cargo run`) は `twigpui-dev` の
  XDG ディレクトリと callback port 8734、release ビルドは `twigpui` と 8733 を使う
  (`src/profile.rs`)。ディレクトリ名・ポート・ウィンドウタイトルを足したり変えたり
  するときは、両プロファイルで衝突しないことをテストで押さえる。
  切り替えは `debug_assertions` のみで、フラグも環境変数も無い。
  課金する既定値をプロファイルで分けるときも `src/profile.rs` に置く
  (list_id の既定と `--sync-list` の同期元がそう)。本番のデータを触る操作は
  `cargo run --release` か `.app` から実行する。
- **`.env` は編集不可**: パーミッション設定により Claude セッションからは `.env` を
  読み書きできない。認証情報が要るときは環境変数を export してもらう。
  `.env` は cwd で効くのでプロファイルを跨ぐ。dev 専用の設定は
  `~/.config/twigpui-dev/config.toml` に置く。
