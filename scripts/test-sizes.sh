#!/usr/bin/env bash
# テストのサイズ (small / medium / large) をファイル単位で分類し､large を
# ゲートする｡
#
# 今日のピラミッドは崩れていない — 1023 本が 2.8〜5.2 秒で終わり､子プロセスを
# 起動するテストは 1 本､ネットワークを叩くテストは 0 本だ｡是正するものは無く､
# **この状態を保つ仕組み** が無いことだけが課題だった｡だからこのスクリプトが
# 見るのは「速いか」ではなく「何に触っているか」になる｡
#
# 時間を測らないのは意図してのことだ｡1 テストごとの時間は stable では取れない
# (`--report-time` は nightly のまま､nextest は別途インストールする別ツール)｡
# 5 秒の suite にそれは過剰だし､壁時計はランナーの機嫌で動く｡触っているものは
# 動かない｡
#
# cargo も呼ばない｡`scripts/coverage.sh` を sandbox の中で走らせると cargo が
# 起こす `ps` が Operation not permitted で落ちる — 測るためにビルドする道具は
# 環境を選ぶ｡`code-metrics.sh` と同じく bash と awk だけで組んであるので､
# インストールも実行も要らない｡
#
# 境界は `code-metrics.sh` / `coverage.sh` と同じで､各 `.rs` の最初の
# `^#\[cfg(test)\]` 行より後がテスト領域だ｡このリポジトリで「実装」と
# 「テスト」の定義は 1 つでよい｡fn の境界は切らない: awk で切ったものは
# rustfmt の気分で壊れるし､ファイル単位でも「このファイルは何に触るか」は
# 答えられる｡
#
# 判定するマーカー (テスト領域内｡コメントだけの行は見ない):
#
# | サイズ | マーカー |
# | --- | --- |
# | large | `Command::new`, `thread::sleep`, `reqwest`, `ureq`, `read(std::process::id())` |
# | medium | `env::temp_dir`, `TcpListener`, `#[gpui::test]`, `TestAppContext`, `XClient::new` |
# | small | 上のどれも無い |
#
# 2 つ､実際のソースに合わせてある:
#
# - `XClient::new` は large ではなく medium だ｡`src/ui/mod.rs` のテストが
#   ダミーのトークンで 1 つ作っているが､コンストラクタは何も送らない
#   (そのテストのコメント自身がそう書いている)｡実際に送る経路は `ureq` を
#   通るので､large はそちらで捕まえる｡構築だけを large と呼ぶと初日から
#   落ちるチェックになり､直されずに無効化される｡
# - `read(std::process::id())` という綴りで 1 箇所を名指ししている｡
#   `src/perf.rs` のテストは `/bin/ps` を起動するが､`Command::new` は同じ
#   ファイルの実装側にあってテスト領域には現れない｡実装のヘルパー越しに
#   子プロセスへ届くテストは静的には見えない､というのがこの分類の限界で､
#   既知のこの 1 件を呼び出しの綴りで留めてある｡綴りが変われば allowlist の
#   エントリが空振りし､`--check` はそれを失敗として報告する (下記)｡
#
# 2 つのモード:
#
# - 引数なし: markdown のレポートを出す｡CI が step summary へ追記する｡
# - `--check`: allowlist に無い large があれば非ゼロで終了し､どのファイルの
#   どのマーカーかを stderr に出す｡それ以外は何も出さない｡
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

# large を許す (ファイル, マーカー) の組｡タブ区切りで､理由を隣に書く｡
allowlist=(
  # `/bin/ps` を本当に読めることの検証｡`perf::read` はこの 1 本でしか
  # 動かず､中身の parse は別のテストが担う｡sandbox の中では
  # Operation not permitted で落ちるが CI では通る｡
  $'src/perf.rs\tread(std::process::id())'
)

# allowlist に置けないマーカー｡suite の壁時計とネットワークは､理由を書けば
# 通る形にしない｡待つテストは待ち続けるし､ネットワークを叩くテストは課金と
# 断線を CI に持ち込む｡
never_allowed=('thread::sleep' 'reqwest' 'ureq')

# allowlist 自身の検算｡置けないマーカーが入っていれば､どのファイルを見るより
# 先に落とす｡
validate_allowlist() {
  local entry marker never
  # macOS の `/bin/bash` は 3.2 で､`set -u` の下では空配列の展開が
  # unbound variable になる｡allowlist は空になりうる — 空にできることが
  # このゲートの目的だ｡だから数を先に見る｡
  [ "${#allowlist[@]}" -gt 0 ] || return 0
  for entry in "${allowlist[@]}"; do
    marker="${entry#*$'\t'}"
    for never in "${never_allowed[@]}"; do
      if [ "$marker" = "$never" ]; then
        # shellcheck disable=SC2016  # バッククォートはメッセージの一部でサブシェルではない
        printf 'error: `%s` cannot be allowlisted. Rewrite the test instead.\n' \
          "$marker" >&2
        exit 2
      fi
    done
  done
}

# ファイルごとに 1 行: パス､`#[test]` 本数､`#[gpui::test]` 本数､サイズ､
# 根拠になったマーカー (カンマ区切り)｡テスト領域を持たないファイルは出さない｡
#
# shellcheck disable=SC2016  # 下の `$0` は awk のフィールドで shell のものではない
classify() {
  find src -name '*.rs' -print0 |
    xargs -0 awk '
      function flush(   size, markers) {
        if (file == "" || !has_tests) return
        size = "small"
        markers = ""
        if (large_hits != "") { size = "large"; markers = large_hits }
        else if (medium_hits != "") { size = "medium"; markers = medium_hits }
        printf "%s\t%d\t%d\t%s\t%s\n", file, tests, gpui_tests, size, markers
      }

      BEGIN {
        large_n = split("Command::new|thread::sleep|reqwest|ureq|read(std::process::id())", large, "|")
        medium_n = split("env::temp_dir|TcpListener|#[gpui::test]|TestAppContext|XClient::new", medium, "|")
      }

      FNR == 1 {
        flush()
        file = FILENAME
        in_tests = 0; has_tests = 0; tests = 0; gpui_tests = 0
        large_hits = ""; medium_hits = ""
        split("", seen)
      }

      /^#\[cfg\(test\)\]/ { in_tests = 1; has_tests = 1; next }
      !in_tests { next }

      /^[[:space:]]*#\[test\]/ { tests++ }
      /^[[:space:]]*#\[gpui::test\]/ { gpui_tests++ }

      # コメントだけの行は見ない｡doc コメントはマーカーの名前をよく挙げる｡
      /^[[:space:]]*\/\// { next }

      {
        for (i = 1; i <= large_n; i++) {
          if (index($0, large[i]) && !seen[large[i]]++) {
            large_hits = large_hits (large_hits == "" ? "" : ",") large[i]
          }
        }
        for (i = 1; i <= medium_n; i++) {
          if (index($0, medium[i]) && !seen[medium[i]]++) {
            medium_hits = medium_hits (medium_hits == "" ? "" : ",") medium[i]
          }
        }
      }

      END { flush() }
    ' |
    sort
}

if [ "${1:-}" = "--check" ]; then
  validate_allowlist

  failed=0

  # 配列は 1 度だけ改行区切りの文字列へ移し､以降はそれを引く｡bash 3.2 の
  # `set -u` は空配列の展開を unbound variable にするので､展開する場所は
  # 少ないほうがいい (`validate_allowlist` を参照)｡
  entries=""
  if [ "${#allowlist[@]}" -gt 0 ]; then
    printf -v entries '%s\n' "${allowlist[@]}"
  fi

  # allowlist のどのエントリが実際に効いたか｡空振りしたエントリは､許して
  # いたはずのものが消えたか綴りが変わったということで､これも失敗にする｡
  # 黙って無害になった allowlist は､次に本物の large が来たときに何も守らない｡
  used=""

  while IFS=$'\t' read -r file _tests _gpui size markers; do
    [ "$size" = "large" ] || continue
    IFS=',' read -r -a hits <<<"$markers"
    for marker in "${hits[@]}"; do
      key="$file"$'\t'"$marker"
      case $'\n'"$entries" in
      *$'\n'"$key"$'\n'*)
        used="$used$key"$'\n'
        ;;
      *)
        # shellcheck disable=SC2016  # バッククォートはメッセージの一部でサブシェルではない
        printf '%s: large test marker `%s`. A test that spawns a process, sleeps, or reaches the network belongs somewhere else — rewrite it, or add the file and marker to the allowlist in %s and say why in the pull request.\n' \
          "$file" "$marker" "scripts/test-sizes.sh" >&2
        failed=1
        ;;
      esac
    done
  done < <(classify)

  while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    case $'\n'"$used" in
    *$'\n'"$entry"$'\n'*) continue ;;
    esac
    printf 'allowlist entry matched nothing: %s (%s). The test is gone or its spelling changed. Drop the entry, or fix the spelling.\n' \
      "${entry%%$'\t'*}" "${entry#*$'\t'}" >&2
    failed=1
  done <<<"$entries"

  if [ "$failed" -ne 0 ]; then
    exit 1
  fi
  exit 0
fi

if [ -n "${1:-}" ]; then
  printf 'usage: %s [--check]\n' "$0" >&2
  exit 2
fi

validate_allowlist

report=$(classify)

printf '## Test sizes\n\n'
# shellcheck disable=SC2016  # バッククォートは markdown であってサブシェルではない
printf '| File | `#[test]` | `#[gpui::test]` | Size | Markers |\n'
printf '| --- | ---: | ---: | --- | --- |\n'

printf '%s\n' "$report" |
  awk -F'\t' '
    {
      markers = $5
      if (markers == "") {
        markers = "—"
      } else {
        gsub(/[^,]+/, "`&`", markers)
        gsub(/,/, ", ", markers)
      }
      printf "| %s | %s | %s | %s | %s |\n", $1, $2, $3, $4, markers
    }
  '

# shellcheck disable=SC2016  # 同上
printf '\n| Size | Files | `#[test]` | `#[gpui::test]` |\n'
printf '| --- | ---: | ---: | ---: |\n'

printf '%s\n' "$report" |
  awk -F'\t' '
    { files[$4]++; tests[$4] += $2; gpui[$4] += $3 }
    END {
      n = split("small medium large", order, " ")
      for (i = 1; i <= n; i++) {
        size = order[i]
        printf "| %s | %d | %d | %d |\n", size, files[size], tests[size], gpui[size]
        total_files += files[size]; total_tests += tests[size]; total_gpui += gpui[size]
      }
      printf "| **total** | **%d** | **%d** | **%d** |\n", total_files, total_tests, total_gpui
    }
  '

# shellcheck disable=SC2016  # バッククォートは markdown であってサブシェルではない
printf '\nlarge は `scripts/test-sizes.sh --check` が落とす (allowlist は同じ'
printf 'スクリプトの中｡理由つき)｡サイズの定義と､なぜ時間ではなくマーカーで'
# shellcheck disable=SC2016  # 同上
printf '見るのかもそこに書いてある｡記録は `docs/test-sizes.md`｡\n'
