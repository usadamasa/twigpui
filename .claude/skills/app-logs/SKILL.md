---
name: app-logs
description: >-
  twigpui が何をしたかをログで確かめるときに使う。Finder から起動した `.app` の
  挙動がおかしい、`cargo run` のログが見つからない、ログのレベルを上げたい、
  ログに書いたはずの 1 行が無い、ログにトークンが漏れていないか確かめたい、
  といったときが対象。`--fetch-only` などヘッドレスの出力がログに無いときにも引く。
  ログの書き方 (`log::info` 等) を足すときの規則は `src/log.rs` の doc が担当。
---

# app-logs

`.app` には stderr が無い (#40, #45)。起動してから挙動がおかしくなったセッションの
手がかりはログファイルにしかない。

## 場所

| 起動 | ログ |
| --- | --- |
| release (`.app`, `cargo run --release`) | `~/.local/state/twigpui/logs/twigpui.log` |
| debug (`cargo run`, `twigpui-dev.app`) | `~/.local/state/twigpui-dev/logs/twigpui.log` |

`XDG_STATE_HOME` を変えていればその下。`cache` ではなく `state` なのは、
次の起動で消えるログでは昨日の問いに答えられないから。

```sh
tail -f ~/.local/state/twigpui/logs/twigpui.log
```

**debug ビルドは別ファイル (#169)。** `cargo run` の挙動を追っているのに
`twigpui/` の下を読んでいると、何も出ていないように見える。

ターミナルから起動したときは stderr にも流れる。**stderr はレベルで絞られない**
(`log::write` はレベルの判定より前に echo する) ので、ターミナルに出た `DEBUG` の行が
ファイルに無いのは正常。`.app` から起動したときはファイルだけ。

## 起動の行から読む

起動のたびに `starting twigpui <version> (<commit>)` が 1 行入る (#231)。
どのビルドの挙動かはここで確かめる。`-dirty` が付いていれば未コミットの変更入り。

## レベル

`error` / `warn` / `info` / `debug`。既定は `info`。`off` は無い。

| 手段 | 効く起動 |
| --- | --- |
| 環境変数 `TWIGPUI_LOG=debug` | ターミナルからの `cargo run` だけ |
| `config.toml` の `log_level = "debug"` | すべて |

**Finder・Spotlight・Dock から起動した `.app` はシェルの環境変数を見ない**
(`app-bundle`)。`.app` のレベルを変えるなら `config.toml`。両方あれば環境変数が勝つ。
認識できない値は起動を止めず、警告して既定に落ちる。

dev の `config.toml` は `~/.config/twigpui-dev/config.toml`。

## ログに無いもの

- **トークン。** 書く前に `log::redact` を通る: `Bearer <token>`、および
  `access_token=` / `refresh_token=` / `client_secret=` / `token=` / `code=` / `state=`
  の値は `[redacted]` になる。伏せすぎに倒してあるので、値が要る調査はログでは
  できない。ファイル自体も `0600`。
  **JSON 形 (`"access_token":"…"`) は伏せない** (#246)。token エンドポイントの
  本文をそのままログに出す行は書かない。
- **ヘッドレスの出力。** `--fetch-only` / `--fetch-post` / `--usage` は stderr へ
  直接書き、ファイルには残さない。ターミナルの出力を保存する。
- **1 MiB より前。** 超えた時点で `twigpui.log.1` へ移して新しく始める。
  残るのは 1 世代だけ。それより前は無い。
- **fixture 由来の取得失敗。** `--fixture` はネットワークへ出ない (#234) ので、
  `media fetch failed` / `avatar fetch failed` の WARN は本物の起動のもの。

## ログの行を足すとき

`log::info` / `log::warn` / `log::error` に文字列を渡す。フレームワークは使わない
(理由は `src/log.rs` の doc)。書いた行が secret を含みうるなら、`redact` の
テストにその形を足す。
