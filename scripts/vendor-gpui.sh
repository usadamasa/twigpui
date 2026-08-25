#!/usr/bin/env bash
# `vendor/gpui` を作り直す: crates.io の gpui <version> の正規化ソースを
# 写し､`vendor/gpui.patch` を当てる｡なぜ vendor しているかは Cargo.toml の
# `[patch.crates-io]` の上のコメントを参照｡
#
# 使い方:
#
#   scripts/vendor-gpui.sh <version>          # 作り直す (gpui を上げるとき)
#   scripts/vendor-gpui.sh <version> --diff   # 今の vendor/gpui と registry の
#                                             # 差分を stdout へ (patch の更新用)
#
# registry の展開済みソース (`$CARGO_HOME/registry/src/*/gpui-<version>`) を
# 読むので､先にその版が取得されていなければならない｡`[patch.crates-io]` が
# 効いていると `cargo fetch` は registry の gpui を取りに行かないので､版を
# 上げるときは Cargo.toml の patch を一時的にコメントアウトして `cargo fetch`
# し､その後この script を走らせる｡
#
# docs/ examples/ tests/ Cargo.lock Cargo.toml.orig は写さない｡path
# dependency として build するのに要らず､3MB の src だけで十分だからだ｡
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

version="${1:-}"
mode="${2:-}"
if [ -z "$version" ]; then
  printf 'usage: %s <gpui version> [--diff]\n' "$0" >&2
  exit 2
fi

cargo_home="${CARGO_HOME:-$HOME/.cargo}"
registry=""
for candidate in "$cargo_home"/registry/src/*/"gpui-$version"; do
  if [ -d "$candidate" ]; then
    registry="$candidate"
    break
  fi
done
if [ -z "$registry" ]; then
  printf 'error: gpui %s is not in %s/registry/src — comment out [patch.crates-io] and run cargo fetch first\n' \
    "$version" "$cargo_home" >&2
  exit 1
fi

vendored="vendor/gpui"
keep=(src resources build.rs Cargo.toml LICENSE-APACHE README.md)

if [ "$mode" = "--diff" ]; then
  # vendor 側にあるファイルだけを比べる｡写していない docs/ などが「消えた」
  # として並ぶのを避けるためで､patch は写したものの上にしか当たらない｡
  while IFS= read -r file; do
    # diff は差分ありで 1 を返し､それはここで期待する結果だ｡2 以上は
    # 読めなかったということで､それだけを失敗にする｡
    diff_status=0
    diff -u --label "a/$file" --label "b/$file" "$registry/$file" "$vendored/$file" \
      || diff_status=$?
    if [ "$diff_status" -gt 1 ]; then
      printf 'error: could not diff %s (diff exit %s)\n' "$file" "$diff_status" >&2
      exit 1
    fi
  done < <(cd "$vendored" && find . -type f | sed 's|^\./||' | sort)
  exit 0
fi

if [ "$mode" != "" ]; then
  printf 'error: unknown mode %s\n' "$mode" >&2
  exit 2
fi

if [ ! -f "$vendored.patch" ]; then
  printf 'error: %s.patch is missing\n' "$vendored" >&2
  exit 1
fi

staging=$(mktemp -d "$repo_root/tmp/vendor-gpui.XXXXXX")
# mktemp は 0700 で作る｡vendor に置くものなので普通のディレクトリに戻す｡
chmod 755 "$staging"
for entry in "${keep[@]}"; do
  cp -R "$registry/$entry" "$staging/"
done
if ! patch -p1 -d "$staging" <"$vendored.patch"; then
  printf 'error: %s.patch does not apply to gpui %s — rebase the patch\n' "$vendored" "$version" >&2
  exit 1
fi
rm -rf "$vendored"
mv "$staging" "$vendored"
printf 'vendored gpui %s into %s with %s.patch applied\n' "$version" "$vendored" "$vendored"
