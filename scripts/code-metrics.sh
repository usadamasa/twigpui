#!/usr/bin/env bash
# この crate の構造メトリクスを報告する (#48)｡
#
# #47 が問うのは「この式は正しいか」だ｡こちらが問うのは「構造が崩れて
# いないか」 — ファイルサイズ､関数の長さ､認知的複雑度｡
#
# 意図して bash と awk と clippy だけで組んである｡#48 の候補ツール
# (rust-code-analysis, cargo-modules, jscpd, cargo-bloat) はどれも CI の
# 実行ごとにインストール手順を要求するし､#46 はビルド時間についての open な
# issue だ｡誰も待たないレポートのほうが､push のたびに遅くなる代わりに得られる
# より良いレポートより価値がある｡それらのツールのどれかがインストール時間に
# 見合うようになったとき､置き換えられるのがこのスクリプトだ｡
#
# 2 つのモード:
#
# - 引数なし: レポートを出す｡メトリクスで失敗することはない｡
# - `--check`: 実装行数を `metrics-baseline.tsv` と突き合わせ､天井を超えた
#   ファイルか baseline にまったく載っていないファイルがあれば非ゼロで終了
#   する｡それ以外は何も出さない｡
#
# この分け方は意図的だ｡関数の長さはすでに `clippy::too_many_lines`
# (`pedantic` 経由で deny､#47) が強制しているし､認知的複雑度はゲートに
# するヒットが無い｡だからファイルサイズが､実際の問題があって既存のチェックも
# 無い唯一のメトリクスになる — しかもここから誰も届かない理想ではなく､
# 今日の数字に対するラチェットでゲートする｡なぜその形なのかは
# `metrics-baseline.tsv` を参照｡
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

baseline_file="metrics-baseline.tsv"

# $1 の実装行数 — `#[cfg(test)]` 行より前のすべて､それが無ければファイル
# 全体｡この crate はテストを同じファイルに置いており､60% がテストの
# ファイルは､60% が描画コードのファイルと同じ問題ではない｡
implementation_lines() {
  local file="$1" total match
  total=$(wc -l <"$file" | tr -d ' ')

  # grep は「一致なし」で 1 を､本当の失敗で 2 を返す｡ここで普通の結果と
  # 言えるのは前者だけだ — テストモジュールが無いファイル｡だから両方を
  # `|| true` で飲み込まずに見分ける｡飲み込めば､読めないファイルが
  # 「テストが無い」として隠れてしまう｡
  if match=$(grep -n -m1 '^#\[cfg(test)\]' "$file"); then
    printf '%s\n' "$(( ${match%%:*} - 1 ))"
  else
    local status=$?
    if [ "$status" -ne 1 ]; then
      printf 'error: could not read %s (grep exit %s)\n' "$file" "$status" >&2
      exit 1
    fi
    printf '%s\n' "$total"
  fi
}

if [ "${1:-}" = "--check" ]; then
  failed=0
  while IFS= read -r file; do
    impl=$(implementation_lines "$file")

    # 行に対する grep ではなく､タブ区切りのフィールドに対する awk を使う:
    # フィールドの完全一致なら `src/like.rs` が `src/liked.rs` で満たされる
    # ことはないし､macOS が積む BSD grep には `\t` 用の `-P` が無い｡パスが
    # 無いとき awk は非ゼロで終了し､それがここでの合図になる｡
    if ! ceiling=$(awk -F'\t' -v want="$file" \
      '$1 == want { print $2; found = 1 } END { exit !found }' "$baseline_file"); then
      printf '%s is not in %s (%s implementation lines). Add it.\n' \
        "$file" "$baseline_file" "$impl" >&2
      failed=1
      continue
    fi

    if [ "$impl" -gt "$ceiling" ]; then
      printf '%s: %s implementation lines, over its ceiling of %s. Split it, or raise the ceiling in %s and say why in the pull request.\n' \
        "$file" "$impl" "$ceiling" "$baseline_file" >&2
      failed=1
    fi
  done < <(find src -name '*.rs' | sort)

  if [ "$failed" -ne 0 ]; then
    exit 1
  fi
  exit 0
fi

# --- ファイルサイズ ---------------------------------------------------------
#
# 総行数ではなく実装行数だ: この crate はテストを同じファイルに置いており､
# 60% がテストのファイルは､60% が描画コードのファイルと同じ問題ではない｡
# 区切りは `#[cfg(test)]` 行で､rustfmt はこのコードベースでそれを必ず
# 0 桁目に置く｡

printf '## File sizes\n\n'
printf '| File | Total | Implementation | Tests |\n'
printf '| --- | ---: | ---: | ---: |\n'

while IFS= read -r file; do
  total=$(wc -l <"$file" | tr -d ' ')
  impl_lines=$(implementation_lines "$file")
  test_lines=$((total - impl_lines))
  printf '| %s | %s | %s | %s |\n' "$file" "$total" "$impl_lines" "$test_lines"
done < <(find src -name '*.rs' | sort)

# --- 最も長い関数 -----------------------------------------------------------
#
# rustfmt は関数の閉じ括弧をその `fn` とちょうど同じインデントに置く｡それが
# パーサ無しで測れる理由だ｡作りからして近似ではある: 行を数えるので､長い
# match の腕と gpui のビルダ呼び出しの長い連鎖が同じ重みになる｡ここでは
# それが正しい近似だ — `ui.rs` の長さの問題はビルダ連鎖にある｡

printf '\n## Longest functions (implementation only)\n\n'
printf '| Lines | Function | File |\n'
printf '| ---: | --- | --- |\n'

# shellcheck disable=SC2016  # 下の `$0`/`$1` は awk のフィールドで shell のものではない
find src -name '*.rs' -print0 |
  xargs -0 awk '
    FNR == 1 { in_tests = 0 }
    /^#\[cfg\(test\)\]/ { in_tests = 1 }
    in_tests { next }
    {
      if (open_at == 0 && $0 ~ /^[[:space:]]*(pub\([a-z]+\) )?(async )?fn [a-zA-Z_]/) {
        match($0, /^[[:space:]]*/)
        indent = RLENGTH
        close_brace = sprintf("%*s}", indent, "")
        if (indent == 0) close_brace = "}"
        open_at = FNR
        name = $0
        sub(/^[[:space:]]*/, "", name)
        sub(/^(pub\([a-z]+\) )?(async )?fn /, "", name)
        sub(/[(<].*$/, "", name)
        file = FILENAME
        next
      }
      if (open_at != 0 && $0 == close_brace) {
        printf "%d\t%s\t%s\n", FNR - open_at + 1, name, file
        open_at = 0
      }
    }
  ' |
  sort -rn |
  # `head` ではなく `sed -n '1,15p'`: `head` はパイプを早く閉じるので､
  # `set -o pipefail` の下では `sort` が SIGPIPE で失敗し､他を何も出力
  # しないうちにスクリプト全体が落ちる｡
  sed -n '1,15p' |
  awk -F'\t' '{ printf "| %s | `%s` | %s |\n", $1, $2, $3 }'

# --- 認知的複雑度 -----------------------------------------------------------
#
# nursery の lint なので､Cargo.toml で deny せずここで報告する: #47 のルールは
# `#[allow]` を生む lint は採用しない､というものだ｡これはその判断を下す前に
# 数字を見ておく必要がある｡

printf '\n## Cognitive complexity (clippy::cognitive_complexity)\n\n'

# clippy は crate 単位でキャッシュするので `touch` する: これが無いと同じ CI
# ジョブ内の 2 回目の実行が何も報告せず､この節が綺麗に見えてしまう｡
touch src/main.rs
if ! complexity=$(cargo clippy --all-targets --message-format=json --quiet -- \
  -W clippy::cognitive_complexity 2>&1); then
  # shellcheck disable=SC2016  # この書式文字列は printf のもので､下で展開される
  printf 'clippy failed; complexity not measured. Its output:\n\n```\n%s\n```\n' "$complexity"
  exit 0
fi

hits=$(printf '%s\n' "$complexity" |
  jq -r 'select(.reason == "compiler-message")
         | select(.message.code.code == "clippy::cognitive_complexity")
         | "\(.message.spans[0].file_name):\(.message.spans[0].line_start)"' |
  sort -u)

if [ -z "$hits" ]; then
  printf 'No function exceeds clippy'"'"'s default threshold.\n'
else
  printf '%s\n' "$hits" | sed 's/^/- /'
fi
