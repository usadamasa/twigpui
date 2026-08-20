---
name: rust-lint-gauntlet
description: >-
  clippy が `arithmetic_side_effects` / `indexing_slicing` / `string_slice` で弾いたとき、
  テストの中だけ lint を緩めたいとき、新しい lint を足すか判断するときに使う。
  このリポジトリは lint をすべて deny にしているので、回避ではなく書き直しで通す。
---

# rust-lint-gauntlet

`Cargo.toml` の `[lints.rust]` / `[lints.clippy]` は `unsafe_code` を forbid し、
それ以外を **すべて `deny`** にしている。`warn` に下げると手元で通って CI だけが落ちるので、
**レベルを下げて回避しない**。バイナリクレートなので公開項目は `pub(crate)` で書く。

## `arithmetic_side_effects` — `+` `-` `*` がそのまま書けない

飽和・ラップ・チェックのどれかを選び、**なぜそれで良いかをコメントに書く**。

| 選択肢 | 使う場面 |
| --- | --- |
| `saturating_*` | 溢れたら端で止まってよいとき (既定の選択) |
| `wrapping_*` | 範囲が型の中に収まると証明できるとき |
| `checked_*` | 溢れを失敗として扱いたいとき |

コメントが要るのは、この値が信用できないから。タイムスタンプは時計と API ヘッダー由来、
バイト位置はリダイレクト URL 由来で、**どれもこのコードが決めた値ではない**。
「明らかに溢れない」と書く前に、その値がどこから来たかを辿る。

## `indexing_slicing` / `string_slice` — パニックする添字が書けない

リモート入力 (API レスポンス、投稿本文、OAuth のリダイレクト) を扱う箇所では
`&s[..n]` ではなく `s.get(..n)` を使う。

この 3 つの入り口はどれも、過去に実際のパニックを出した経路。
「ここは短い文字列だから大丈夫」は、相手がこちらの都合に合わせてくれる前提になっている。

## テストの中だけの許可

`main.rs` の `#![cfg_attr(test, allow(...))]` **1 行**で
`unwrap_used` / `expect_used` / `indexing_slicing` / `string_slice` / `panic` を許可している。
テストの添字とパニックは表明であって事故ではないため。

**許可を増やすときはこの 1 行に足す。個別の `#[allow]` を撒かない。**
撒き始めると、どこがテストの都合でどこが本番コードの妥協なのか区別できなくなる。

## 新しい lint を足すとき

1. まず有効にして件数を測る。
2. 採否の理由を `Cargo.toml` のコメントに残す。
3. **`#[allow]` を撒くことになる lint は採用しない。** 例外が要る lint は、
   その lint がこのコードベースに合っていないということ。

## 手元で CI と同じものを通す

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
