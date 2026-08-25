#!/usr/bin/env bash
# twigpui.app を組み立てる: release ビルド､Info.plist､任意のアイコン､そして
# ad-hoc の署名 (#40)｡Rust の依存は増やさない (cargo-bundle は使わない) —
# .app はディレクトリの配置にすぎず､100 行ほどの shell で組めば全体が読める
# ままになる｡
#
# `--dev` (#169) を付けると代わりに twigpui-dev.app を組む: *debug* ビルド､
# 専用の bundle id､彩度を落としたアイコン｡debug なのは見た目の都合ではない —
# `Profile::current` は `debug_assertions` を読むので､開発用の XDG ディレクトリと
# 開発用の OAuth callback port を指すのはまさに debug ビルドだ｡dev の bundle を
# `--release` で組むと､見た目は開発用なのに本番インストールのファイルへ書き込む
# アプリができてしまう｡
#
# 署名は ad-hoc (`codesign -s -`) のみ｡プロジェクトの non-goals に従う: これは
# 開発専用､単一マシン向けのビルドだ｡notarize もしないし配布も想定していない｡
# ただし ad-hoc 署名だけは必要だ — まったく署名されていない bundle は､
# ビルドしたてのバイナリに対して Gatekeeper が即座に拒否する｡
set -euo pipefail

log() {
  printf '%s\n' "$*" >&2
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    log "ERROR: required command not found on PATH: $1"
    exit 1
  fi
}

# bundle の組み立て途中ではなく､最初に大きな音で失敗させる｡
for cmd in cargo jq plutil codesign; do
  require_cmd "$cmd"
done

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

# cargo のパッケージ名｡プロファイルによって変わらない — 下で読む metadata の
# エントリと､cargo が target/ に置くバイナリの両方の名前になる｡
CRATE_NAME="twigpui"

# --- プロファイルの選択 (#169) ---
#
# 認識できない引数を無視せず拒否するのが要点だ: `--dev` の打ち間違いが黙って
# 本番の bundle を組んでしまうと､正しく見える名前で誤ったアプリが dist/ に
# 置かれることになる｡
DEV=0
for arg in "$@"; do
  case "$arg" in
    --dev) DEV=1 ;;
    *)
      log "ERROR: unknown argument: $arg"
      log "usage: $0 [--dev]"
      exit 1
      ;;
  esac
done

if [ "$DEV" -eq 1 ]; then
  APP_NAME="twigpui-dev"
  BUNDLE_ID="com.github.usadamasa.twigpui.dev"
  CARGO_PROFILE_DIR="debug"
else
  APP_NAME="$CRATE_NAME"
  BUNDLE_ID="com.github.usadamasa.twigpui"
  CARGO_PROFILE_DIR="release"
fi

DIST_DIR="$REPO_ROOT/dist"
APP_BUNDLE="$DIST_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
PLIST="$CONTENTS_DIR/Info.plist"
ICON_NAME="AppIcon.icns"
ICON_ICNS_SRC="$REPO_ROOT/assets/AppIcon.icns"
ICON_PNG_SRC="$REPO_ROOT/assets/AppIcon.png"

# --- バージョン｡手で保守する二つ目のコピーを持たず Cargo.toml から
# `cargo metadata` 経由で読む｡パース用の依存も足さない (`cargo metadata` は
# 最初から cargo に付いてくるし､jq はありふれた CLI ツールであって､この
# バイナリがリンクする crate ではない)｡ ---
metadata=$(cargo metadata --no-deps --format-version=1 --manifest-path "$REPO_ROOT/Cargo.toml")
if ! err=$(printf '%s' "$metadata" | jq empty 2>&1); then
  log "ERROR: cargo metadata did not produce valid JSON: $err"
  exit 1
fi
version=$(printf '%s' "$metadata" | jq -r --arg name "$CRATE_NAME" \
  '.packages[] | select(.name == $name) | .version')
if [ -z "$version" ]; then
  log "ERROR: package \"$CRATE_NAME\" not found in cargo metadata output"
  exit 1
fi
log "twigpui version: $version"

# --- ビルド ---
log "building the $CARGO_PROFILE_DIR binary..."
# 追加引数の配列を使わず分岐ごとに書き下している: 空の配列を `set -u` の下で
# 展開すると､macOS に付属する bash 3.2 では "unbound variable" エラーになる｡
# このスクリプトが動くのはその shell だ｡
if [ "$DEV" -eq 1 ]; then
  (cd "$REPO_ROOT" && cargo build)
else
  (cd "$REPO_ROOT" && cargo build --release)
fi
BIN_PATH="$REPO_ROOT/target/$CARGO_PROFILE_DIR/$CRATE_NAME"
if [ ! -x "$BIN_PATH" ]; then
  log "ERROR: expected $CARGO_PROFILE_DIR binary not found or not executable: $BIN_PATH"
  exit 1
fi

# --- bundle の骨組み ---
log "assembling $APP_NAME.app..."
rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cp "$BIN_PATH" "$MACOS_DIR/$APP_NAME"
chmod +x "$MACOS_DIR/$APP_NAME"

# --- アイコン (任意) ---
#
# ここは「警告してスキップせず大きな音で失敗させる」への意図的な例外だ:
# アイコンが無いのは前提条件の欠落ではなく､明示的に任意の asset である
# (このスクリプトを書いたセッションで実際の絵が用意されなかった理由は
# README.md と twigpui #40 のレポートにある)｡ここで防ごうとしている失敗は
# *dangling* な参照 — コピーされていないファイルを CFBundleIconFile が
# 指している状態 — であって､キーそのものの不在ではない｡後者は macOS が
# 汎用のアプリアイコンにフォールバックして処理する｡
#
# `--dev` (#169) は二つ目の手描きファイルを持たず､同じ絵の彩度を落とす:
# 単一の source of truth を単一のまま保てるし､色付きの隣に並ぶ灰色のアイコンは
# Dock のサイズで「同じアプリの開発用コピー」と読める｡この派生にはピクセルが
# 要るので､ビルド済みの .icns がそこにあっても dev の bundle は PNG の経路を
# 通る — .icns は release の bundle が同梱するものであり､使い回せば本番の
# アプリのアイコンが開発用に付いてしまう｡
have_icon=0
if [ "$DEV" -eq 0 ] && [ -f "$ICON_ICNS_SRC" ]; then
  log "using existing icon: $ICON_ICNS_SRC"
  cp "$ICON_ICNS_SRC" "$RESOURCES_DIR/$ICON_NAME"
  have_icon=1
elif [ -f "$ICON_PNG_SRC" ]; then
  require_cmd sips
  require_cmd iconutil
  log "building $ICON_NAME from $ICON_PNG_SRC..."
  iconset_dir=$(mktemp -d "${TMPDIR:-/tmp}/twigpui-iconset.XXXXXX") ||
    { log "ERROR: mktemp -d failed"; exit 1; }
  trap 'rm -rf "$iconset_dir"' EXIT
  mkdir -p "$iconset_dir/AppIcon.iconset"

  icon_source="$ICON_PNG_SRC"
  if [ "$DEV" -eq 1 ]; then
    log "desaturating the icon for the development bundle..."
    # 意図して 2 パスに分け､in place ではなく 2 つのファイルへ書く｡1 回目は
    # 画像を gray の ColorSync プロファイルに合わせて色を落とす｡2 回目は
    # RGB へ戻す｡`iconutil` が求めるのは普通の RGBA PNG で､1 チャンネルの
    # gray PNG はそれではないからだ｡結果はチャンネルがたまたま等しい RGB
    # 画像になる — 見た目は灰色で､構造は iconutil が期待するものだ｡
    sips -m "/System/Library/ColorSync/Profiles/Generic Gray Profile.icc" \
      "$ICON_PNG_SRC" --out "$iconset_dir/AppIcon-gray.png" >/dev/null
    sips -m "/System/Library/ColorSync/Profiles/sRGB Profile.icc" \
      "$iconset_dir/AppIcon-gray.png" --out "$iconset_dir/AppIcon-dev.png" >/dev/null
    icon_source="$iconset_dir/AppIcon-dev.png"
  fi

  # iconutil が要求する size:label の組 — `iconutil --help` と Apple Human
  # Interface Guidelines のアイコンサイズ表を参照｡
  for spec in \
    16:icon_16x16 32:icon_16x16@2x \
    32:icon_32x32 64:icon_32x32@2x \
    128:icon_128x128 256:icon_128x128@2x \
    256:icon_256x256 512:icon_256x256@2x \
    512:icon_512x512 1024:icon_512x512@2x; do
    px="${spec%%:*}"
    label="${spec##*:}"
    sips -z "$px" "$px" "$icon_source" \
      --out "$iconset_dir/AppIcon.iconset/$label.png" >/dev/null
  done
  iconutil -c icns "$iconset_dir/AppIcon.iconset" -o "$RESOURCES_DIR/$ICON_NAME"
  have_icon=1
elif [ "$DEV" -eq 1 ] && [ -f "$ICON_ICNS_SRC" ]; then
  log "NOTE: the development bundle derives its icon from assets/AppIcon.png," \
    "which is missing — assets/AppIcon.icns is the release icon and is not" \
    "reused here, so this builds without a custom icon."
else
  log "NOTE: no icon source at assets/AppIcon.icns or assets/AppIcon.png —" \
    "building without a custom icon (macOS shows the generic app icon)." \
    "See README.md for how to add one."
fi

# --- Info.plist｡手書きの XML テンプレートではなく plutil で組み立てる｡
# その場しのぎの文字列置換ではなく､すべての値が plutil 自身のエスケープを
# 通るようにするためだ｡ ---
plutil -create xml1 "$PLIST"
plutil -insert CFBundleIdentifier -string "$BUNDLE_ID" "$PLIST"
plutil -insert CFBundleName -string "$APP_NAME" "$PLIST"
plutil -insert CFBundleExecutable -string "$APP_NAME" "$PLIST"
plutil -insert CFBundlePackageType -string APPL "$PLIST"
plutil -insert CFBundleInfoDictionaryVersion -string 6.0 "$PLIST"
plutil -insert CFBundleShortVersionString -string "$version" "$PLIST"
plutil -insert CFBundleVersion -string "$version" "$PLIST"
plutil -insert NSHighResolutionCapable -bool YES "$PLIST"
if [ "$have_icon" -eq 1 ]; then
  plutil -insert CFBundleIconFile -string "$ICON_NAME" "$PLIST"
fi
plutil -lint "$PLIST" >/dev/null

# --- ad-hoc 署名 ---
log "signing (ad-hoc)..."
codesign --force -s - "$APP_BUNDLE"
if ! codesign --verify --strict "$APP_BUNDLE"; then
  log "ERROR: codesign verification failed"
  exit 1
fi

log "built: $APP_BUNDLE"
