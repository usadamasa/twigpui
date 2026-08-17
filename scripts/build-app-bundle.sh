#!/usr/bin/env bash
# Assemble twigpui.app: a release build, an Info.plist, an optional icon, and
# an ad-hoc code signature (#40). No new Rust dependency (no cargo-bundle) —
# a .app is just a directory layout, and building it in ~100 lines of shell
# keeps the whole thing legible.
#
# Ad-hoc signing (`codesign -s -`) only, per the project's non-goals: this is
# a development-only, single-machine build. It is not notarized and is not
# meant to be distributed. Ad-hoc signing is still required, though — an
# entirely unsigned bundle is refused outright by Gatekeeper on a freshly
# built binary.
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

# Fail loudly up front rather than partway through assembling the bundle.
for cmd in cargo jq plutil codesign; do
  require_cmd "$cmd"
done

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

APP_NAME="twigpui"
BUNDLE_ID="com.github.usadamasa.twigpui"
DIST_DIR="$REPO_ROOT/dist"
APP_BUNDLE="$DIST_DIR/$APP_NAME.app"
CONTENTS_DIR="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"
PLIST="$CONTENTS_DIR/Info.plist"
ICON_NAME="AppIcon.icns"
ICON_ICNS_SRC="$REPO_ROOT/assets/AppIcon.icns"
ICON_PNG_SRC="$REPO_ROOT/assets/AppIcon.png"

# --- version, read from Cargo.toml via `cargo metadata` rather than a
# second hand-maintained copy, and without adding a parsing dependency
# (`cargo metadata` already ships with cargo; jq is a common CLI tool, not a
# crate this binary links against). ---
metadata=$(cargo metadata --no-deps --format-version=1 --manifest-path "$REPO_ROOT/Cargo.toml")
if ! err=$(printf '%s' "$metadata" | jq empty 2>&1); then
  log "ERROR: cargo metadata did not produce valid JSON: $err"
  exit 1
fi
version=$(printf '%s' "$metadata" | jq -r --arg name "$APP_NAME" \
  '.packages[] | select(.name == $name) | .version')
if [ -z "$version" ]; then
  log "ERROR: package \"$APP_NAME\" not found in cargo metadata output"
  exit 1
fi
log "twigpui version: $version"

# --- release build ---
log "building the release binary..."
(cd "$REPO_ROOT" && cargo build --release)
BIN_PATH="$REPO_ROOT/target/release/$APP_NAME"
if [ ! -x "$BIN_PATH" ]; then
  log "ERROR: expected release binary not found or not executable: $BIN_PATH"
  exit 1
fi

# --- bundle skeleton ---
log "assembling $APP_NAME.app..."
rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cp "$BIN_PATH" "$MACOS_DIR/$APP_NAME"
chmod +x "$MACOS_DIR/$APP_NAME"

# --- icon (optional) ---
#
# This is a deliberate exception to "fail loudly instead of warning and
# skipping": a missing icon is not a missing prerequisite, it is an
# explicitly optional asset (see README.md and the twigpui #40 report for
# why no real artwork was produced in the session that wrote this script).
# The failure mode being guarded against here is a *dangling* reference —
# CFBundleIconFile pointing at a file that was never copied in — not the
# absence of the key itself, which macOS handles by falling back to the
# generic app icon.
have_icon=0
if [ -f "$ICON_ICNS_SRC" ]; then
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
  # size:label pairs iconutil requires — see `iconutil --help` / the
  # Apple Human Interface Guidelines icon size table.
  for spec in \
    16:icon_16x16 32:icon_16x16@2x \
    32:icon_32x32 64:icon_32x32@2x \
    128:icon_128x128 256:icon_128x128@2x \
    256:icon_256x256 512:icon_256x256@2x \
    512:icon_512x512 1024:icon_512x512@2x; do
    px="${spec%%:*}"
    label="${spec##*:}"
    sips -z "$px" "$px" "$ICON_PNG_SRC" \
      --out "$iconset_dir/AppIcon.iconset/$label.png" >/dev/null
  done
  iconutil -c icns "$iconset_dir/AppIcon.iconset" -o "$RESOURCES_DIR/$ICON_NAME"
  have_icon=1
else
  log "NOTE: no icon source at assets/AppIcon.icns or assets/AppIcon.png —" \
    "building without a custom icon (macOS shows the generic app icon)." \
    "See README.md for how to add one."
fi

# --- Info.plist, built with plutil rather than a hand-written XML template
# so every value goes through plutil's own escaping instead of ad hoc string
# substitution. ---
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

# --- ad-hoc signature ---
log "signing (ad-hoc)..."
codesign --force -s - "$APP_BUNDLE"
if ! codesign --verify --strict "$APP_BUNDLE"; then
  log "ERROR: codesign verification failed"
  exit 1
fi

log "built: $APP_BUNDLE"
