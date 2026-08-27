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
# - `--check`: 実装行数を一律の上限 (下の `max_implementation_lines`) と
#   突き合わせ､超えたファイルがあれば非ゼロで終了する｡上限より大きかった
#   ファイルは `metrics-baseline.tsv` に天井つきで残っていて､その天井は
#   `origin/main` と比べて下がる方向にしか動かせない｡それ以外は何も出さない｡
#
# この分け方は意図的だ｡関数の長さはすでに `clippy::too_many_lines`
# (`pedantic` 経由で deny､#47) が強制しているし､認知的複雑度はゲートに
# するヒットが無い｡だからファイルサイズが､実際の問題があって既存のチェックも
# 無い唯一のメトリクスになる｡
#
# 上限が一律なのは #241 の帰結だ｡#48 から #241 までの 8 日間､天井は
# ファイルごとにあり､同じ pull request で上げれば通った｡引き上げ 78 回に
# 対して引き下げは 8 回で､TSV は理由の散文が 307 行､数字が 49 行になった｡
# 「レビュアーの目に入る」は､書く人とレビューする人が同じなら止める力に
# ならない｡だから天井は上げられないものにする｡ここで通す道は 2 つだけ —
# ファイルを分割するか､この数字をこのファイルで上げるか｡後者は全ファイルに
# 効く 1 か所の変更で､pull request の diff で見落としようがない｡
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

baseline_file="metrics-baseline.tsv"

# 1 ファイルの実装行数の上限｡テスト行は数えない (`implementation_lines`)｡
#
# 600 は今日の分布から置いた｡49 ファイルのうち 38 がすでに下にいて､上に
# いるのは分割の途中か､分割を後回しにしたものだ｡この crate は doc が
# 重く (天井の引き上げ理由の大半は「増えた分は doc」だった)､500 だと
# 普通の 1 モジュールがぶつかる｡800 だと #241 が名指した 4 ファイルを
# 割った先がそのまま 800 に張りつく｡
max_implementation_lines=600

# 天井の比較先｡ローカルの clone には必ずあり､CI では Lint ジョブが
# `--check` の前に fetch する (`.github/workflows/ci.yaml`)｡
base_ref="origin/main"

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

# $2 の TSV から $1 の天井を出す｡載っていなければ非ゼロ｡
#
# 行に対する grep ではなく､タブ区切りのフィールドに対する awk を使う:
# フィールドの完全一致なら `src/like.rs` が `src/liked.rs` で満たされる
# ことはないし､macOS が積む BSD grep には `\t` 用の `-P` が無い｡パスが
# 無いとき awk は非ゼロで終了し､それがここでの合図になる｡
ceiling_of() {
  awk -F'\t' -v want="$1" \
    '$1 == want { print $2; found = 1 } END { exit !found }' "$2"
}

if [ "${1:-}" = "--check" ]; then
  failed=0

  # --- 1. 今日のツリーを上限と突き合わせる ---------------------------------
  while IFS= read -r file; do
    impl=$(implementation_lines "$file")

    if [ "$impl" -le "$max_implementation_lines" ]; then
      # 上限の下にいるファイルに天井は要らない｡残っていれば､次に太った
      # ときに上限ではなく古い天井で通ってしまう｡
      if ceiling=$(ceiling_of "$file" "$baseline_file"); then
        printf '%s: %s implementation lines, under the cap of %s, but still listed in %s with a ceiling of %s. Remove the entry.\n' \
          "$file" "$impl" "$max_implementation_lines" "$baseline_file" "$ceiling" >&2
        failed=1
      fi
      continue
    fi

    if ! ceiling=$(ceiling_of "$file" "$baseline_file"); then
      printf '%s: %s implementation lines, over the cap of %s. Split it.\n' \
        "$file" "$impl" "$max_implementation_lines" >&2
      failed=1
      continue
    fi

    if [ "$impl" -gt "$ceiling" ]; then
      printf '%s: %s implementation lines, over its ceiling of %s. Split it. The ceiling cannot be raised.\n' \
        "$file" "$impl" "$ceiling" >&2
      failed=1
    fi
  done < <(find src -name '*.rs' | sort)

  # --- 2. 天井の一覧を base と突き合わせる ---------------------------------
  #
  # 一覧は縮むことしかできない: 載っているファイルは base にも載っていて､
  # 天井は base 以下｡消えたファイルの行も残せない｡
  if ! base_baseline=$(git show "$base_ref:$baseline_file"); then
    printf 'error: cannot read %s from %s. Fetch it first: git fetch origin main\n' \
      "$baseline_file" "$base_ref" >&2
    exit 1
  fi

  while IFS=$'\t' read -r file ceiling; do
    case "$file" in '#'* | '') continue ;; esac

    if [ ! -f "$file" ]; then
      printf '%s is listed in %s but does not exist. Remove the entry.\n' \
        "$file" "$baseline_file" >&2
      failed=1
      continue
    fi

    if ! base_ceiling=$(ceiling_of "$file" <(printf '%s\n' "$base_baseline")); then
      printf '%s is new in %s (ceiling %s). The list only shrinks; split the file instead.\n' \
        "$file" "$baseline_file" "$ceiling" >&2
      failed=1
      continue
    fi

    if [ "$ceiling" -gt "$base_ceiling" ]; then
      printf '%s: ceiling raised from %s to %s in %s. Ceilings only go down; split the file instead.\n' \
        "$file" "$base_ceiling" "$ceiling" "$baseline_file" >&2
      failed=1
    fi
  done <"$baseline_file"

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
printf 'Cap: %s implementation lines per file (--check). Files still above it, and their ceilings, are in %s.\n\n' \
  "$max_implementation_lines" "$baseline_file"
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
