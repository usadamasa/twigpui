---
name: app-bundle
description: >-
  twigpui を `.app` として組むときに使う。Spotlight・Launchpad・Dock から起動したい、
  アイコンを差し替えたい、dev 用の bundle を本番と並べて置きたいときが対象。
  Finder から起動したら設定が読まれない、`cargo run` では出ていた画面が出ない、
  アイコンが汎用のものになる、といった症状にも引く。
  ウィンドウの見た目を確認するだけなら fixture-visual-check を使う。
---

# app-bundle

`cargo run` はこのチェックアウトのターミナルからしかウィンドウを開けない。
Spotlight・Launchpad・Dock から見えるものが要るなら bundle を組む。

```sh
./scripts/build-app-bundle.sh          # dist/twigpui.app     (release, 本番プロファイル)
./scripts/build-app-bundle.sh --dev    # dist/twigpui-dev.app (debug,   dev プロファイル)
```

スクリプトがやること: 対応するプロファイルのバイナリをビルドし、`Info.plist` を書き、
ad-hoc 署名 (`codesign -s -`) する。`CFBundleVersion` /
`CFBundleShortVersionString` は `cargo metadata` から読むので、
`Cargo.toml` の `version` を二重管理しない。

置き場所は自由。

```sh
mv dist/twigpui.app /Applications/
```

**ad-hoc 署名しかしない。** このプロジェクトの非目標 (macOS 専用・開発用途、
notarization も Developer ID も無し) に沿ったもので、配布可能にするためではなく、
自分のマシンでビルドした bundle を Gatekeeper が門前払いしないためだけにある。

## dev と本番を並べて置く (#169)

`--dev` は **debug ビルド**を包む。これは手抜きではなく仕様で、
`Profile::current` が `debug_assertions` を読む以上、
dev の XDG ディレクトリと callback port を選ぶのは debug ビルドであることそのもの。
`--release` で dev bundle を組むと、dev の名前とアイコンを持ちながら
本番のファイルを書くアプリができる。プロファイル分離が防ごうとしている当のもの。

| | `dist/twigpui.app` | `dist/twigpui-dev.app` |
| --- | --- | --- |
| ビルド | release | debug |
| bundle id | `com.github.usadamasa.twigpui` | `com.github.usadamasa.twigpui.dev` |
| 実行ファイル名 | `twigpui` | `twigpui-dev` |
| アイコン | `assets/AppIcon.png` | 同じ絵を彩度落とししたもの |

両方インストールして構わない。実行ファイル名が違うので、
`open` も `cleanshot-capture --app twigpui-dev` も取り違えない。

プロファイルが何を分けているか、dev の client_id をどこに置くかは
[README の Development builds](../../../README.md#development-builds) にある。

## アイコン

`assets/AppIcon.png` が原本 (#85)。スクリプトが `iconutil` の要求する 10 サイズへ
リサイズして `.icns` を bundle へ書く。`sips` も `iconutil` も macOS 同梱なので
入れるものは無い。

- `assets/AppIcon.icns` を置いてあれば、**本番ビルドだけ**それをそのまま使う。
- `--dev` は `.icns` を使わない。灰色アイコンは `sips` で PNG から作るので、
  ビルド済みの `.icns` (= 本番の絵) を dev bundle に載せることはしない。
- どちらの原本も無ければ `CFBundleIconFile` を書かず、macOS が汎用アイコンを出す。
  コピーしていないファイルへの参照を書き残すことはしない。

`sandbox` 内では `sips` が必ず失敗する (`$TMPDIR` ではなく `/var/folders` へ
中間ファイルを書くため)。Claude のセッションからは検証できないので、
アイコンまわりを変えたらユーザーに `! ./scripts/build-app-bundle.sh --dev` を
実行してもらう。詳細は `sandbox-troubleshooting` スキル。

## Finder から起動する前に読む

**Finder・Spotlight・Dock から起動したプロセスはシェルの環境変数を引き継がない。**
作業ディレクトリもこのチェックアウトではない。したがって、シェルの profile で export した
`X_OAUTH_CLIENT_ID` や `X_TARGET_USERNAME`、リポジトリの `.env` は
bundle 起動には一切効かない。ここから 2 つ出てくる。

- **`oauth_client_id` は `config.toml` に置く。** 非秘匿の値なので
  `$XDG_CONFIG_HOME/twigpui/config.toml` (既定 `~/.config/twigpui/config.toml`)
  に置けば起動方法によらず読まれる。`HOME` は launchd が起動する全プロセスに
  設定されており、`Paths::from_env` が要るのはそれだけ (`XDG_*` が未設定なら、
  何も export していないターミナル起動と同じ既定に落ちる)。
  一度サインインすれば session は `$XDG_STATE_HOME/twigpui/oauth_tokens.json`
  に残るので、次の bundle 起動でも環境変数は要らない。
- **`config.toml` に `bearer_token` が残っていると起動が失敗する** (#33)。
  読まれないキーを黙って無視すると「設定できている」と思い込んだまま何も読まれない
  状態になるので、`oauth_client_id` を代替として名指しするエラーで落とす。
  値そのものはメッセージに出さない。

設定エラーはどこを見ればいいか名指しする。以前は stderr にしか出ず、
ターミナルを持たない bundle 起動では黙って終了していた。今は stderr が端末でないとき、
解決済みの `config.toml` のパスを名指しするネイティブアラート
(`osascript … display alert`) も出す。

## bundle 起動で変わるもの・変わらないもの

- **OAuth の loopback listener は変わらない。** bundle でもそうでなくても
  `127.0.0.1:8733` (dev ビルドは `8734`、#169) を bind する。
  `oauth::callback` は作業ディレクトリにもシェル環境にも依存せず、
  ポートが空いていることしか要求しない。
  ただし **新しく署名された**バイナリが listen socket を bind する初回は、
  macOS が「"twigpui" が外部からの接続を受け入れようとしています」を出しうる。
  ad-hoc 署名はリビルドごとに署名 identity が変わる (トークンを Keychain に置かない
  のと同じ理由) ので、このプロンプトはリビルドのたびに再登場しうる。
- **`macos-blade` と WindowServer も変わらない。** Finder/Dock/Spotlight から
  起動した `.app` は Terminal.app から起動したバイナリと同じ通常のユーザー
  WindowServer セッションを持つ。bundle であること自体はこの接続に影響しない。
  これは bundle スクリプトを書いた環境からは検証できなかった。sandbox は
  WindowServer に届かず (`gpui` は `cargo run` でも `NoSupportedDeviceFound` で
  panic する)、bundle のレイアウト・`Info.plist`・署名の確認まではできたが、
  実際に起動することはできない。**ウィンドウが開くことを確かめられるのは、
  組み上がった `.app` を人がダブルクリックしたときだけ。**
