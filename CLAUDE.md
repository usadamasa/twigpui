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
**立てる前に既存の issue を検索する。** 過去に重複を作った実績がある (#69 → #65)。

## 開発ポリシー

### 開発体験の改善は優先度キューに並べず、随時やる

ビルド時間 (#46)、Linter の厳格化 (#47)、コードスメルの自動検知 (#48)、
ログ出力 (#49) のような開発体験・保守性の課題は、
優先度ラベルの順番を待たせない。機能追加の作業中に手が届いたらその場で拾う。

後回しにすると効き目が落ちる性質のものだから。Linter が緩いままコードが増えれば
後から厳しくする手間が増える。ログが無いまま調査を重ねれば、同じ調査を何度も
やることになる。

ただし機能追加の PR に混ぜない。別コミット・別 PR に分ける。

### 当たり前の機能を優先する

X クライアントとして当然あるべき操作 (読む、書く、反応する、外部で開く) を、
体験の改善や別フロントエンドより前に置く。backlog (#4) は読み取り中心の計画だったので、
いいね・画像表示・外部への導線といった基本機能が漏れている。
「issue が無い」は「やらなくてよい」ではない。

## プロジェクト構成

| パス | 役割 |
| --- | --- |
| `src/main.rs` | エントリポイント、`--fetch-only` の分岐 |
| `src/config.rs` | 環境変数 / `.env` の読み込みと検証 |
| `src/x_api/model.rs` | API レスポンス型、投稿と作者の join |
| `src/x_api/client.rs` | ureq によるブロッキングクライアント、ステータス→メッセージ変換 |
| `src/oauth/` | OAuth 2.0 Authorization Code + PKCE (#7): PKCE 生成、ループバックリスナー、トークン永続化 |
| `src/compose.rs` | 投稿コンポーザーの純粋ロジック (#14): 文字数カウント、下書き検証、送信状態遷移 |
| `src/ui.rs` | gpui のウィンドウとタイムライン描画 |
| `scripts/build-app-bundle.sh` | `.app` バンドル生成 (#40): リリースビルド、`Info.plist`、任意アイコン、ad-hoc 署名 |
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
  forbid し、それ以外は **すべて `deny`** にしている (#47)。`warn` にすると手元で通って
  CI だけが落ちるので、レベルを下げない。バイナリクレートなので公開項目は `pub(crate)`
  で書く。
  - オーバーフローは `arithmetic_side_effects` で禁止している。`+` / `-` / `*` を
    そのまま書けない。飽和 (`saturating_*`)、範囲が保証されている場合の `wrapping_*`、
    失敗を扱う `checked_*` のどれかを選び、**なぜそれで良いかをコメントに書く**。
    タイムスタンプは時計と API ヘッダー由来、バイト位置はリダイレクト URL 由来で、
    どれもこのコードが信用してよい値ではない。
  - パニックを生む添字・スライスは `indexing_slicing` / `string_slice` で禁止している。
    リモート入力 (API レスポンス、投稿本文、OAuth のリダイレクト) を扱う箇所では
    `&s[..n]` ではなく `s.get(..n)` を使う。#47 でこの 3 箇所から実際にパニックが出た。
  - テストの中だけは `main.rs` の `#![cfg_attr(test, allow(...))]` 1 行で
    `unwrap_used` / `expect_used` / `indexing_slicing` / `string_slice` / `panic` を
    許可している。テストの添字とパニックは表明であって事故ではない。
    **許可を増やすときはこの 1 行に足す。個別の `#[allow]` を撒かない。**
  - lint を足すときは、まず有効にして件数を測り、採否の理由を `Cargo.toml` の
    コメントに残す。`#[allow]` を撒くことになる lint は採用しない。
- **構造の計測**: `scripts/code-metrics.sh` がファイルサイズ・関数長・認知的複雑度を
  出す (#48)。CI の Lint ジョブが毎回走らせて run summary に貼る。
  - **ファイルサイズだけは CI が落とす**。`scripts/code-metrics.sh --check` が
    `metrics-baseline.tsv` の天井 (実装行数、今日の値を 50 行単位で切り上げたもの)
    と突き合わせる。超えたら分割するか、**同じ PR で天井を上げて理由を書く**。
    黙って育てられないことが目的で、上げること自体は正当な操作。
    ファイルが縮んだら天井も下げる。**新規ファイルは先に登録しないと落ちる**。
  - 関数長は `clippy::too_many_lines` (`pedantic` 経由で deny、#47) が既に見ている。
    認知的複雑度は clippy の既定閾値で 1 件も出ないので、報告のみ。
  - 新しい計測ツールを足すときは、CI での毎回のインストール時間に見合うかを
    先に確かめる (#46)。
- **課金に直結する API**: X API はプリペイド残高からの従量課金。リロード 1 回で
  ID 解決 (単一ユーザーモードは screen name 検索、ホームタイムラインモードは `/users/me`) +
  タイムライン取得の 2 リクエストを消費する (#11)。「Load older」のクリックごとにさらに
  1 リクエスト。ポーリングや自動更新を足すときは必ず取得間隔とキャッシュをセットで設計する。
  残高が尽きると `429` の `UsageCapExceeded` が返る。
- **テストはネットワークを叩かない**: パースとエラー変換をフィクスチャ JSON で検証する。
  テストが課金を発生させないことを保ちつづける。
- **`.env` は編集不可**: パーミッション設定により Claude セッションからは `.env` を
  読み書きできない。認証情報が要るときは環境変数を export してもらう。

## ホームタイムライン表示 (#11) と単一ユーザー取得 (#33 以降)

ウィンドウは常にホームタイムラインを表示する。
`GET /2/users/:id/timelines/reverse_chronological` は OAuth 2.0 Authorization Code
(ユーザーコンテキスト) しか受け付けないが、**#33 でアプリ専用 Bearer トークンを
廃止したので、資格情報は 1 種類しかない**。表示先を資格情報の種類で分岐させていた
`TimelineSource::for_credential` は削除済みで、分岐そのものが無くなった。

- 表示: `GET /2/users/me` で自分の id を取得し (#9 と同じ仕組みでキャッシュ)、
  `GET /2/users/:id/timelines/reverse_chronological` を引く。「Load older」ボタンで
  `meta.next_token` を辿って過去方向に追加取得できる。
- `--fetch-only`: `GET /2/users/:id/tweets` で `X_TARGET_USERNAME` の投稿を引く。
  **資格情報としての Bearer を落とすことと、単一ユーザー取得を落とすことは別**で、
  このエンドポイントは OAuth トークンでも普通に叩ける (#24 のパネルでも使う)。

ホームタイムラインと単一ユーザーのキャッシュは同じ user id でも内容が異なるため、
`Paths::home_timeline_file` / `Paths::timeline_file` として別ファイルに分けている。
レートリミットも `/users/me` とホームタイムラインをそれぞれ独立した `Endpoint` として
追跡する (#10 の仕組みを流用)。
