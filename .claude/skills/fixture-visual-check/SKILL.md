---
name: fixture-visual-check
description: >-
  twigpui の見た目を手元で確認するときに使う。色・フォント・字の詰まり方を見る、
  変更前後を見比べる、添付の並びを目で確かめるときが対象。
  課金なしで毎回同じ画面を出す `--fixture` 起動と課金される素の起動の使い分け、
  出した画面の撮り方・見方を扱う。
  確認したいケースをフィクスチャに足すときにも引く。
  間隔・位置・クリックの当たりは撮らずにテストで押さえられるので、
  撮る前にどちらの話かを見分けるときにも使う。
  画面がロックされたまま (離席中に) 撮りたいとき、撮ったら真っ黒だったときにも引く。
---

# fixture-visual-check

色・フォント・字の詰まり方は実際に描かせて見るしかない。
撮って初めて分かることのために、課金なしで毎回同じ画面を出す。

**この手順は手元専用。CI では回さない。**

## まず撮らずに済まないかを考える (#184)

**寸法はテストで押さえられる。** 長らくこのスキルは「gpui はテストプラットフォームで
レイアウトを走らせない」と書いていたが、これは誤りだった。no-op なのは
`TestWindow::draw` (`Scene` をピクセルにする段) だけで、`Window::draw` は
`prepaint_as_root` を通るのでレイアウトエンジンは走る。

要素に `render::Addressable` で名前を付けると `VisualTestContext::debug_bounds`
がその要素の実際の bounds を返す。間隔・位置・重なり・要素の有無はここから
assert できる。`simulate_click` は hit test も通るので、クリックの経路も書ける
(`src/ui/mod.rs` の `clicking_*` を参照)。

撮るのは、テストで書けないと確かめてからにする。

| 見たいもの | 手段 |
| --- | --- |
| 間隔・位置・重なり・折り返しの結果の高さ | `debug_bounds` で assert |
| クリックが要素に当たるか | `simulate_click` + `debug_bounds` |
| 状態遷移・フォーカス | `dispatch_action` (#146 層 3) |
| 色・フォント・字の詰まり方・アイコンの描かれ方 | 撮る (この手順) |

## 手順

```sh
# 1. 決定的なオフライン起動。background で走らせる
cargo run -- --fixture fixtures/timeline.json

# 2. ウィンドウが出たか確かめる
cleanshot-capture list

# 3. 撮る
cleanshot-capture window --app twigpui --out ./tmp/shot.png
```

撮ったら `Read` で開く。撮り方の詳細と、別のウィンドウが写るときの対処は
`cleanshot-capture` スキルに従う。

### dev ビルドと本番ビルドの見分け (#169)

`cargo run` は debug ビルドなので dev プロファイルで動き、ウィンドウタイトルは
`twigpui (dev)` になる。本番の `.app` は `twigpui`。プロセス名はどちらも
`twigpui` (dev の `.app` を組んだときだけ `twigpui-dev`) なので、
本番の `.app` を開いたまま `cargo run` すると `--app twigpui` が両方に当たる。

そのときは `--title` で絞る:

```sh
cleanshot-capture window --app twigpui --title "twigpui (dev)" --out ./tmp/shot.png
```

### `--fixture` と素の起動を使い分ける

**レイアウトを見るなら `--fixture`。** ネットワークにも実キャッシュにも触らず、
資格情報も要らず、毎回同じ行が出る。見比べが成立するのはこれだけ。

**実際のタイムラインが見たいなら素の `cargo run`。** 本物の本文の長さや添付の出方は
フィクスチャでは再現しきれないので、これが要る場面はある。ただし起動だけで
**2 リクエスト課金される** (`x-api-budget`)。見るたびに払うことを承知で叩く。

課金を伴う起動は勝手に増やさない。素で起動する用があるなら、その 1 回で
確認したいことを済ませる。

### 撮るのは 2 の後

`list` にウィンドウが出ていれば最初のフレームは描き終わっている。ただし
**アバターは非同期に届いて隣の行を組み直す**ので、画像まで揃った画面が要るなら
一度撮って確かめ、まだ来ていなければ撮り直す。

### 画面がロックされていても `--fixture` は撮れる (#220)

`--fixture` の window は画面ロック中に起動しても描く。gpui を fork の patch 版から
取っていて (`Cargo.toml` の `[patch.crates-io]`)、fixture 起動だけが
`gpui::set_draw_while_occluded` を入れるからだ。離席中に Claude が見た目を確かめる
ための仕組みで、撮り方は上の手順と同じ。fork をやめる条件は #221、upstream への報告は
zed-industries/zed#63217。

**素の `cargo run` と `.app` はロック中に立てると真っ黒になる。** upstream の gpui は
occluded な window の display link を張らず、ロック中は OS が CVDisplayLink の生成を
拒む (-6661) ので、ロック中に開いた window は 1 フレームも持たない。本番の見た目を
撮るならロック前に立てる。ロック前に描いた最後のフレームはロック中も撮れる。

**撮ったら真っ黒だったとき**は、描画の regression と決めつける前に
`cleanshot-capture area --x 0 --y 0 --width 4 --height 4 --out ./tmp/probe.png` を
1 回叩く。ロック中なら「ロックを解除してから撮り直してください」と返る。
それが出て、かつ fixture 起動なのに黒いなら、patch が外れた
(`[patch.crates-io]` が消えた、`Startup::draws_while_occluded` が false に戻った —
`only_a_fixture_window_keeps_drawing_while_occluded` が守っている) のを疑う。
Ghostty のようなロック中も自前で描くアプリが写ることは対照にならない。

### 終わらせ方

**background job を止める。** sandbox の中ではプロセス一覧が取れないので
(`ps -A` はヘッダーだけ、`pgrep` は `Cannot get process list`)、PID を引いて
`kill` する道が無い。人間が見ているなら `⌘Q` でも同じ。

## 確認したいケースはフィクスチャに足す

`fixtures/timeline.json` はパーサーが作りタイムラインが描くのと同じ型の配列で、
API が返しえないタイムラインは書けない。長い本文、4 枚添付、引用と画像、リポスト、
返信 — **見たいケースを 1 画面に並べておく**と、見比べが 1 回の起動で済む。

行を足したら `cargo test` を通す。フィクスチャの読み込みはテストで検証している。

## 撮れないもの

- **メニュー・ダイアログ**。写すことはできるが、**開く手段が無い**。
  キー入力の合成には Accessibility の許可が別に要る。当面は手動のまま。
- **操作の結果**。この手順はクリックもキー入力もしない。ただし撮る必要も無い。
  `src/ui/mod.rs` の `#[gpui::test]` 群でウィンドウを立て、`simulate_click` で
  実際に座標を当てるか、`dispatch_action` で action を投げる。
  ハンドラの中身をテスト側で書き直すと、配線が外れていても通ってしまうので、
  必ず要素を押すか action を投げること。

## 撮ると何が分かるのか (実例)

2026-08-23、#174 のステータスバーで「`Total: 11 req` と `List sync:` が
くっついて `11 reqList` と読める」という不具合をこの手順で見つけた (#182)。
`gap_3` が効いておらず、`gap_8` まで上げても変わらなかった。

**ただしこれは、撮らなければ分からない不具合ではなかった。**
翌日 (#184) に両方の segment へ名前を付けて `debug_bounds` で読んだところ、
修正前の bounds は `usage ends at 191px, sync starts at 191px` — 隙間ゼロで、
テストが落ちた (`the_status_bars_segments_keep_apart`)。

見つけた手段が撮影だったことと、撮影が唯一の手段であることは別。
**気づいたのが目でも、押さえるのはテストで。**

## ピクセル比較を CI に入れない

- macOS ランナーに画面収録の許可を与える手段が無い。
- 仮に撮れても、OS のバージョンやフォントで差分が出て本物の回帰と区別が付かない。
- スナップショットの更新が「とりあえず承認」になり、守っているつもりで何も守らなくなる。

代わりに、ピクセルにする手前まで — bounds、hit test、フォーカス、状態遷移 — を
assert するウィンドウテストを増やす。**画面を見なくても壊れていると分かるものは、
思っていたよりずっと多い。**
