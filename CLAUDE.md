# CLAUDE.md — twigpui

Rust + [gpui](https://crates.io/crates/gpui) で書いた X (Twitter) タイムラインビューア。
macOS 専用、開発用途のみ。他プラットフォームは考慮しない。

## バックログ運用ルール

#4 の指示に基づく。この節のルールは変更しない。

- バックログに書かれたタスクは分解して個別の issue にする。
- issue にはそれぞれラベルを振る。
- issue 化したら、バックログ issue (#4) の一覧からは削除してよい。
- 優先度は適宜判断する。プロジェクトで管理してもよい。
- #4 の「指示」セクションは更新してはならない。

### ラベル体系

| 種別 | ラベル | 用途 |
| --- | --- | --- |
| 優先度 | `priority:high` / `priority:medium` / `priority:low` | 着手順 |
| 領域 | `area:auth` | 認証・OAuth |
| | `area:api` | X API クライアント、クォータ、レートリミット |
| | `area:timeline` | タイムラインの取得と描画 |
| | `area:ui` | gpui のウィンドウ、レイアウト、テーマ |
| | `area:cache` | ローカルキャッシュと永続化 |
| | `area:config` | 設定とファイル配置 |
| | `area:cost` | API 課金の可視化と抑制 |
| | `area:tui` | ターミナル UI モード |
| 状態 | `blocked` | 他 issue 待ち |
| 種類 | `enhancement` / `documentation` / `research` / `bug` | 既定のラベルに準じる |

新しい issue を立てるときは、優先度ラベルを 1 つと、領域ラベルを最低 1 つ付ける。

## プロジェクト構成

| パス | 役割 |
| --- | --- |
| `src/main.rs` | エントリポイント、`--fetch-only` の分岐 |
| `src/config.rs` | 環境変数 / `.env` の読み込みと検証 |
| `src/x_api/model.rs` | API レスポンス型、投稿と作者の join |
| `src/x_api/client.rs` | ureq によるブロッキングクライアント、ステータス→メッセージ変換 |
| `src/oauth/` | OAuth 2.0 Authorization Code + PKCE (#7): PKCE 生成、ループバックリスナー、トークン永続化 |
| `src/ui.rs` | gpui のウィンドウとタイムライン描画 |
| `PLAN.md` | マイルストーンの記録 |

## ビルドとテスト

```sh
cargo build
cargo test
cargo run                # ウィンドウを開く
cargo run -- --fetch-only  # ヘッドレスで取得結果を標準出力に流す
```

CI (`.github/workflows/ci.yaml`) は `macos-latest` で Test と Lint を走らせ、
`ci-status-check` が両者を集約する。ローカルでも push 前に以下を通す。

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## 実装上の制約

- **`macos-blade` 必須**: gpui の既定ビルドは `xcrun metal` を要求し、これは Command Line
  Tools には含まれない。`Cargo.toml` は `default-features = false` + `macos-blade` で
  ビルドしているので、この指定を外さない。同時に x11 / wayland も落ちるため Linux 向けには
  ビルドできない (CI が macOS ランナーのみなのはこのため)。
- **厳格な lint**: `Cargo.toml` の `[lints.rust]` / `[lints.clippy]` で `unsafe_code` を
  forbid し、`pedantic` / `unwrap_used` / `expect_used` を有効にしている。バイナリクレート
  なので公開項目は `pub(crate)` で書く。
- **課金に直結する API**: X API はプリペイド残高からの従量課金。リロード 1 回で
  ID 解決 (単一ユーザーモードは screen name 検索、ホームタイムラインモードは `/users/me`) +
  タイムライン取得の 2 リクエストを消費する (#11)。「Load older」のクリックごとにさらに
  1 リクエスト。ポーリングや自動更新を足すときは必ず取得間隔とキャッシュをセットで設計する。
  残高が尽きると `429` の `UsageCapExceeded` が返る。
- **テストはネットワークを叩かない**: パースとエラー変換をフィクスチャ JSON で検証する。
  テストが課金を発生させないことを保ちつづける。
- **`.env` は編集不可**: パーミッション設定により Claude セッションからは `.env` を
  読み書きできない。認証情報が要るときは環境変数を export してもらう。

## ホームタイムライン表示と単一ユーザーへのフォールバック (#11)

`GET /2/users/:id/timelines/reverse_chronological` は OAuth 2.0 Authorization Code
(ユーザーコンテキスト) しか受け付けず、アプリ専用 Bearer トークンでは 401 になる。
そのため表示モードは解決した credential の種類でそのまま決まる:

- OAuth セッションでサインイン済み: `GET /2/users/me` で自分の id を取得し (#9 と同じ
  仕組みでキャッシュ)、`GET /2/users/:id/timelines/reverse_chronological` でホーム
  タイムラインを表示する。「Load older」ボタンで `meta.next_token` を辿って過去方向に
  追加取得できる。
- Bearer トークンのみ: 従来どおり `GET /2/users/:id/tweets` で `X_TARGET_USERNAME` の
  投稿を表示する (マイルストーン 1 からの挙動を維持したフォールバック)。

どちらのモードかは `oauth::TimelineSource::for_credential` が credential の種類だけから
決める純粋関数で、`cache.rs` や `ui.rs` に `is_oauth()` 分岐を散らさずに一箇所で決定する。
ホームタイムラインと単一ユーザーのキャッシュは同じ user id でも内容が異なるため、
`Paths::home_timeline_file` / `Paths::timeline_file` として別ファイルに分けている。
レートリミットも `/users/me` とホームタイムラインをそれぞれ独立した `Endpoint` として
追跡する (#10 の仕組みを流用)。
