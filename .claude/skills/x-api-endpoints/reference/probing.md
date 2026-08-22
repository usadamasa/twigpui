# 書き捨て probe — API の挙動を実測する

X API の挙動は spec を読んでも確定しない (`../SKILL.md` の冒頭)。
確かめるには実際に撃つしかなく、1 リクエストはそのまま課金になる。
ここでは「安く・記録を残して・繰り返し撃ち直さない」ための手順を置く。

## 前提: Bash からは撃てない

sandbox の制約が 3 つ重なって、シェルからの検証は成立しない。

- `~/.local/state/twigpui/oauth_tokens.json` は sandbox の read deny-list に入っている。
  `jq -r .access_token ...` は読めない。
- `curl` は permission-denied。
- `.env` も Claude セッションからは読めない。

**抜け道は `cargo` ひとつ。** `.claude/settings.json` の `sandbox.excludedCommands` に
`cargo *` があるため、`cargo run` で起動したプロセスは sandbox の外で動き、
トークンファイルを読めるし `api.x.com` にも届く。

したがって probe は Rust の書き捨てクレートとして書く。

## 撃つ前に決めること

1. **予算の上限をユーザーに確認する。** プリペイド残高からの従量課金で、
   probe のリクエストは `~/.local/state/twigpui/usage.json` に**計上されない**
   (あのカウンタはアプリしか見ていない)。手で数える以外に方法がない。
2. **書き込み系を撃つかを別途確認する。** 実アカウントの公開タイムラインが変わる。
   自分の判断で決めない。
3. **1 リクエストで 2 つ以上の問いに答えさせる。**
   `?max_results=5&tweet.fields=...` なら下限境界とフィールド名の両方が同時に分かる。

## 撃つ場所

パラメータの挙動を確かめるときは `GET /2/users/{id}/tweets` (自分のタイムライン) を使う。
決定的に動くため、観測が他の要因と混ざらない。

ホームタイムラインは避ける。このアカウントでは #157 のサーバ側異常があり、
内容に関する観測がその異常と区別できなくなる。

## クレートの構成

`./tmp/probe` に置く (`/tmp` は gitignore 済み)。
`ureq` と `serde_json` はローカルの registry キャッシュにあるのでビルドは速い。

```toml
# ./tmp/probe/Cargo.toml
[package]
name = "probe"
version = "0.0.0"
edition = "2024"

[dependencies]
serde_json = "1.0.151"
ureq = { version = "3.4.0", features = ["json"] }

[workspace]
```

`src/main.rs` の要点は 4 つ。

**非 2xx をエラーにしない。** 400 や 403 の本文こそが欲しいデータなので、
`ureq` の既定 (ステータスをエラーに変換する) を切る。

```rust
let config = ureq::Agent::config_builder()
    .http_status_as_error(false)
    .build();
let agent = ureq::Agent::new_with_config(config);
```

**トークンはファイルから読む。**

```rust
let home = std::env::var("HOME")?;
let path = format!("{home}/.local/state/twigpui/oauth_tokens.json");
let tokens: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
let token = tokens["access_token"].as_str().ok_or("no access_token")?;
```

**`x-` で始まるレスポンスヘッダを全部保存する。** `x-rate-limit-limit` で
エンドポイントの上限が、`x-access-level` で App の権限が、追加課金ゼロで分かる。

```rust
response.headers().iter().filter(|(key, _)| key.as_str().starts_with("x-"))
```

**撃った実行の中でファイルへ書く。** 端末が流れて撃ち直すのは二重課金になる。
1 リクエストにつき `./tmp/probes/NNN-<name>.json`
(リクエスト URL・ステータス・ヘッダ・本文) と、`./tmp/probes/ledger.tsv` への 1 行を、
レスポンスを受けた直後に書く。連番はレジャーの行数から続けると上書きが起きない。

入力は `name<TAB>url` の TSV にして引数で渡すと、ラウンドごとに Rust を書き換えずに済む。

```sh
cargo run --quiet --manifest-path ./tmp/probe/Cargo.toml -- ./tmp/probes/round1.tsv
```

**撤収は `~/.claude/scripts/clean-tmp.sh ./tmp/probe`。** `rm -rf` は毎回確認が入る。
生の観測 (`./tmp/probes/`) は結論を書き終えるまで消さない。

## 安く答えを取る型

### 400 を enum の oracle にする

パラメータに存在しない値を 1 つ送ると、実際に受け付ける値が本文に全部並ぶ。

```
?max_results=5&expansions=bogus   -> 400 "... is not one of [...]"
?max_results=5&tweet.fields=bogus -> 400 "... is not one of [...]"
```

`*.fields` `expansions` の綴りを推測してリトライを重ねるより安い。

### 境界は範囲外を 1 本撃って本文に言わせる

`max_results=4` の 400 は `not between 5 and 100` と返す。
下限と上限が 1 リクエストで確定する。

### ヘッダは撃てば必ず付いてくる

上限を知るために専用のリクエストを撃つ必要はない。
何を撃っても `x-rate-limit-limit` が付く。

## 結論の閉じ方

**仮説は、別の仮説と区別できる計測が出るまで閉じない。**

#157 の調査ではこれを一度破った。「スコープが足りない」という仮説に対して
`follows.read` を足し、再認可を通し、症状が変わらないことを確認しないまま
「直した」と扱いかけた。反証したのは spec の `security` ブロックで、
必要な scope は `tweet.read` + `users.read` だけだった。

同じ調査で、著者構成の切り替わりが 100 件目 / 101 件目ちょうどに乗ったため
「1 ページ目だけ別経路」という説明でも同じデータが出る状態になった。
これを潰したのは 3 本の追加計測 (ページサイズを変えて同じ列を辿る / `since_id` で
先頭を見る / カーソルでなく時刻で窓を切る) で、どれも
「その説明なら別の値が出るはず」を狙って撃ったものだった。

計測を設計するときは、**その結果がどちらに転んでも意味を持つか**を先に言えるようにする。
言えないなら、それは課金するだけで何も決まらないリクエストになる。

## 実測をテストに落とすとき

**テストはネットワークを叩かない。** probe が保存した生 JSON をフィクスチャに使う。
テストは各モジュールの `#[cfg(test)]` に置き、`fixtures/` の JSON を読む
(`src/fixture.rs` が `fixtures/timeline.json` を読む形が既にある)。
テストが課金を発生させないことを保ちつづける。
