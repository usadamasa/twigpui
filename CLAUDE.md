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
  ユーザー照会 + タイムライン取得の 2 リクエストを消費する。ポーリングや自動更新を足すときは
  必ず取得間隔とキャッシュをセットで設計する。残高が尽きると `429` の
  `UsageCapExceeded` が返る。
- **テストはネットワークを叩かない**: パースとエラー変換をフィクスチャ JSON で検証する。
  テストが課金を発生させないことを保ちつづける。
- **`.env` は編集不可**: パーミッション設定により Claude セッションからは `.env` を
  読み書きできない。認証情報が要るときは環境変数を export してもらう。

## ホームタイムラインを出せていない理由

`GET /2/users/:id/timelines/reverse_chronological` は OAuth 2.0 Authorization Code
(ユーザーコンテキスト) しか受け付けず、アプリ専用 Bearer トークンでは 401 になる。
そのため現状は `GET /2/users/:id/tweets` で単一ユーザーの投稿を表示している。
解消は OAuth 2.0 PKCE の実装待ち。
