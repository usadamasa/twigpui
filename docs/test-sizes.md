# テストサイズと壁時計の記録

`scripts/test-sizes.sh` が出す分類と､`cargo test` の壁時計｡分類の定義と
ゲートの仕組みはスクリプトの先頭コメント｡**測ったら 1 行足す｡**
数字だけを足し､閾値はここでは決めない｡

サイズの列は「ファイル数 / `#[test]` 本数」｡medium の括弧は
`#[gpui::test]` の内数｡

| 日付 | commit | 場所 | 本数 | 壁時計 | small | medium | large |
| --- | --- | --- | ---: | ---: | --- | --- | --- |
| 2026-08-26 | 0b7fb43 | CI Test ジョブ (run 32895769712) | 1023 | 5.22s | 21 / 422 | 16 / 547 (+35 gpui) | 1 / 20 |
| 2026-08-26 | 0b7fb43 | 手元 (macOS, debug) | 1023 | 2.84s | 同上 | 同上 | 同上 |
| 2026-09-19 | 02d8779 | CI Test ジョブ (run 35440235263) | 1271 | 18.24s | — | — | — |
| 2026-09-21 | 893f33d | 手元 (macOS, debug)､gpui-pre へ移行した直後 | 1273 | 60.75s | 31 / 533 | 24 / 613 (+108 gpui) | 1 / 20 |
| 2026-09-21 | 7041584 | 手元 (macOS, debug)､gpui-pre と taffy を `opt-level = 2` | 1273 | 5.38s | 同上 | 同上 | 同上 |

## 読み

- **large は `src/perf.rs` の 1 ファイルだけ**で､allowlist に載っている
  (`/bin/ps` を本当に読めることの検証)｡それ以外に子プロセス・`thread::sleep`・
  ネットワークへ触るテストは無い｡
- **medium の根拠はほぼ `env::temp_dir`**｡書き込み先が temp 配下に閉じている
  ので､並列で走っても互いを踏まない｡gpui の 35 本は `src/ui/mod.rs` に
  集まっていて､こちらは `TestAppContext` が根拠｡
- **スクリプトが数える属性は 1024､libtest が走らせるのは 1023**｡差の 1 本は
  `src/profile.rs` の `#[cfg(debug_assertions)]` と `#[cfg(not(debug_assertions))]`
  で､どちらか一方しかコンパイルされない｡スクリプトは属性を数えるので両方を
  数える｡
- **壁時計を決めるのは `advance_clock` で animation を進める 11 本** (2026-09-21)｡
  `advance_clock(2s)` 1 回が window を 21 フレーム描き直し､その時間は taffy の
  レイアウトと gpui の描画の中にある｡未最適化の gpui-pre では 1 フレーム 350ms
  かかったので､`Cargo.toml` の `[profile.dev.package]` がこの 2 crate だけ最適化する｡
- **速くする対象はテストではない**｡同じ run で Test ジョブは 328 秒かかって
  いて､その内訳はビルド 5 分 21 秒とテスト 5.22 秒｡テストの壁時計を削っても
  CI は速くならない｡
