#!/usr/bin/env bash
# 実装のテストカバレッジを報告する — テストのカバレッジではない｡
#
# `cargo llvm-cov` はコンパイルしたものすべてを計測対象にするが､この crate は
# テストをコードと同じファイルの `#[cfg(test)]` 以下に置いている｡テスト関数は
# 定義上必ず実行されるので covered として数えられ､数字を押し上げる: 計測対象
# 1880 関数のうち 1077 がテスト関数で､実装だけなら 54% のところ見出しの数字は
# 79% と読める (2026-08-24 に計測)｡素の数字を報告することは､テストがテストを
# どれだけ覆っているかを報告することになる｡
#
# そこでここの数字はすべて､`#[cfg(test)]` 行より前で始まる region に絞って
# ある — `code-metrics.sh` が実装行とテスト行を分けるのと同じ境界だ｡2 つの
# スクリプトで「実装」の定義は 1 つ｡
#
# 3 つのモード:
#
# - 引数なし: markdown のレポートを出す (ファイルごとの表と､続いてカバレッジ
#   の低いファイル)｡CI が step summary へ追記するのはこれで､手元でも最初に
#   読むのはこれ｡
# - `--gaps [path-prefix]`: 未カバーの関数を `file:line function` の形で
#   1 行ずつ､ファイル順に出す｡テストがどこに足りないかを判断するために
#   agent が読むのはこのモードだ｡prefix は任意で､1 ファイルや 1 ディレクトリ
#   に絞れる｡
# - `--json`: 絞り込んだファイルごとの数字を JSON で出す｡読むのではなく
#   計算したいもの向け｡
#
# 閾値もゲートも意図して置いていない｡このリポジトリのルール
# (`metrics-baseline.tsv` を参照) は､初日から落ちるチェックは直されずに
# 無効化される､というものだ｡さらにカバレッジの下限には､ファイルサイズの
# ラチェットには無いもう一つの問題がある: このアプリのテスト不能な面は
# 実在し文書化もされている (#115 — メニューと､`Scene` から先のすべて)｡
# 描画コードを足す機能 PR は､それ自体に落ち度が無くても割合を下げる｡
# 成果物は gaps の一覧であって､割合は地図だ｡
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

# 計測付きのビルドを共有の build-dir から外し､cargo-llvm-cov が使う target の
# 下へ戻す｡`.cargo/config.toml` は中間成果物を $CARGO_HOME/build/twigpui へ
# 逃がしているが､それをここにも効かせると計測付きの binary と素の binary が
# 同じ deps/ に並ぶ｡下の remove_stale_objects が説明しているとおり
# cargo-llvm-cov は deps/ の `twigpui-<hash>` を **すべて** llvm-cov に渡すので､
# 計測されていない binary が混ざればレポートが壊れる｡
#
# 渡すのは `llvm-cov-target` ではなくその親だ｡cargo-llvm-cov は受け取った
# build-dir の下に自分で `llvm-cov-target` を作る (target-dir と同じ扱い)｡
# `target/llvm-cov-target` を渡すと
# `target/llvm-cov-target/llvm-cov-target/debug/deps` へ二重にネストし､report が
# 歩く `target/llvm-cov-target/debug` には object が 1 つも無くなる｡
export CARGO_BUILD_BUILD_DIR="$repo_root/target"

work_dir="./tmp/coverage"
cov_json="$work_dir/llvm-cov.json"
cutoffs_json="$work_dir/cutoffs.json"

# `llvm-tools-preview` は通常 rustup から来る｡Homebrew の toolchain には
# rustup が無いので Homebrew の LLVM へフォールバックする — ただしバージョンが
# rustc のものと一致するときだけだ｡profile のフォーマットは LLVM のリリース
# ごとに動くので､食い違うと最初ではなくレポートの途中で失敗する｡
ensure_llvm_tools() {
  if [ -n "${LLVM_COV:-}" ] && [ -n "${LLVM_PROFDATA:-}" ]; then
    return
  fi

  local brew_llvm="/opt/homebrew/opt/llvm/bin"
  if [ ! -x "$brew_llvm/llvm-cov" ]; then
    # フォールバック先が無い｡何が足りないかは cargo-llvm-cov が言う｡
    return
  fi

  local rustc_llvm brew_llvm_version
  rustc_llvm=$(rustc --version --verbose | awk '/^LLVM version:/ { print $3 }')
  brew_llvm_version=$("$brew_llvm/llvm-cov" --version |
    awk '/LLVM version/ { print $NF }')

  if [ "$rustc_llvm" != "$brew_llvm_version" ]; then
    printf 'rustc uses LLVM %s but %s is LLVM %s. Install llvm-tools-preview, or set LLVM_COV and LLVM_PROFDATA.\n' \
      "$rustc_llvm" "$brew_llvm/llvm-cov" "$brew_llvm_version" >&2
    exit 1
  fi

  export LLVM_COV="$brew_llvm/llvm-cov"
  export LLVM_PROFDATA="$brew_llvm/llvm-profdata"
}

# 各ファイルで実装が終わる位置｡`code-metrics.sh` の `implementation_lines` を
# 写したもので､「テストモジュールが無い」(grep exit 1) と本当の読み取り失敗
# (それ以外) を見分けるやり方も含めて同じだ｡
write_cutoffs() {
  local file match status
  {
    printf '['
    local first=1
    while IFS= read -r file; do
      if match=$(grep -n -m1 '^#\[cfg(test)\]' "$file"); then
        cut="${match%%:*}"
      else
        status=$?
        if [ "$status" -ne 1 ]; then
          printf 'error: could not read %s (grep exit %s)\n' "$file" "$status" >&2
          exit 1
        fi
        # テストモジュールが無い: 全行が実装だ｡実在するどのファイルよりも
        # 大きい値なので､何も除外されない｡
        cut=999999
      fi
      [ "$first" -eq 1 ] || printf ','
      first=0
      printf '{"file":"%s","cut":%s}' "$file" "$cut"
    done < <(find src -name '*.rs' | sort)
    printf ']\n'
  } >"$cutoffs_json"
}

# ディスク上の profile が､それが説明していると称するコードより古いかどうか｡
#
# これがレポートを読むコストを下げつつ､内容を決して誤らせないための仕掛けだ｡
# 計測付きのビルドは 1-2 分かかる｡このスクリプトが存在する目的のループ —
# 表を読み､1 ファイルの gaps を並べ､テストを書き､埋まったか確かめる — は､
# 呼び出しのたびに再ビルドすればその代金を 4 回払うことになる｡とはいえ編集前の
# profile を使い回せば古いコードを報告することになり､それは遅いことより悪い｡
# だから mtime が決める｡
profile_is_stale() {
  if [ ! -f "$cov_json" ]; then
    return 0
  fi
  # `-print -quit` はツリー全体を歩かず最初に見つけた新しいファイルで止まる｡
  # 答えは同じで､ファイル数にも左右されない｡
  local newer
  newer=$(find src Cargo.toml Cargo.lock -newer "$cov_json" -print -quit)
  [ -n "$newer" ]
}

# 前のビルドが残したこの crate の実行ファイルを消す｡
#
# cargo-llvm-cov の `report` は `target/llvm-cov-target/debug` を丸ごと歩き､
# `twigpui-<hash>` に一致する実行ファイルを **すべて** llvm-cov に渡す
# (`src/report.rs` の `object_files`｡mtime も hash も見ない)｡hash は依存や
# フラグから決まるので､Cargo.lock を更新したり RUSTFLAGS を変えたりすると
# 新しい hash の binary が隣に並び､古い方は消えずに残る｡古い binary が
# 別のソースから作られていれば llvm-cov は同じ関数として畳めず､実装の
# region を二重に数える — 2026-08-26 に手元で 13,146 region のところが
# 24,487 と出た原因がこれで､残っていたのは 8/24 の計測が作った binary
# だった｡CI も #227 から Coverage の target をキャッシュしているので､
# main の Cargo.lock が動いた次の run から同じ形で膨らむ｡
#
# `cargo llvm-cov clean --workspace` は依存の成果物まで落として毎回一から
# 建て直す｡消すべきなのはこの crate の実行ファイルだけで､それは次の
# ビルドが必ず link し直す｡消えるのは link 1 回分の時間だけだ｡
remove_stale_objects() {
  local deps="target/llvm-cov-target/debug/deps"
  [ -d "$deps" ] || return 0
  # `-perm -u+x` で `.d` `.rlib` `.rmeta` を除く｡cargo-llvm-cov 自身の
  # `is_object` と同じ見分け方だ｡
  find "$deps" -maxdepth 1 -type f -perm -u+x -name 'twigpui-*' -delete
}

# 計測付きのテストを走らせ､そのデータから報告する｡`report` は再ビルドせずに
# profile を読み直すので､下の 3 つのモードは 1 回のビルドを共有する｡
#
# `COVERAGE_REUSE=1` は profile が古くても再利用を強制する｡CI は､自分で
# 組み立てたばかりの profile に対する 2 回目と 3 回目のパスでこれを設定する｡
# その間にツリーが変わりようがないからだ｡
measure() {
  ensure_llvm_tools
  mkdir -p "$work_dir"

  if [ -n "${COVERAGE_REUSE:-}" ]; then
    if [ ! -f "$cov_json" ]; then
      printf 'COVERAGE_REUSE is set but %s does not exist. Run without it first.\n' \
        "$cov_json" >&2
      exit 1
    fi
    printf 'Reusing the existing profile (COVERAGE_REUSE is set).\n' >&2
  elif profile_is_stale; then
    printf 'Building instrumented and running the tests (1-2 minutes).\n' >&2
    remove_stale_objects
    cargo llvm-cov --locked --all-targets --no-report
  else
    printf 'Reusing the profile in %s; nothing under src has changed since.\n' \
      "$work_dir" >&2
  fi

  cargo llvm-cov report --json --output-path "$cov_json"
  write_cutoffs
}

# jq には両方のファイルを食わせ､ソースファイルごとに 1 行を出す｡最初の
# region が cutoff より前で始まる関数が実装に属する｡`regions[i][4]` は実行
# 回数なので､それが 0 でなければその region は covered だ｡
#
# `strings` はファイル名を守っている: llvm の JSON は機械が書いたものだが､
# ここではその中の 1 フィールドをパスで読んでおり､そこが文字列でなければ
# 1 行を飛ばすのではなくフィルタ全体が落ちてしまう｡
per_file_json() {
  jq -n \
    --slurpfile cuts "$cutoffs_json" \
    --slurpfile cov "$cov_json" \
    --arg root "$repo_root/" '
      ($cuts[0] | map({key: .file, value: .cut}) | from_entries) as $cut
      | [ $cov[0].data[0].functions[]
          | (.filenames[0] | strings | ltrimstr($root)) as $f
          | select($f != null and $cut[$f] != null)
          | select(.regions[0][0] < $cut[$f])
          | {file: $f, regions: .regions, name: .name, line: .regions[0][0]} ]
      | group_by(.file)
      | map({
          file: .[0].file,
          total: ([.[] | .regions[]] | length),
          covered: ([.[] | .regions[] | select(.[4] != 0)] | length)
        })
      | map(. + {percent: (if .total == 0 then 100 else (10000 * .covered / .total | round) / 100 end)})
      | sort_by(.percent)
    '
}

case "${1:-}" in
--gaps)
  measure >&2
  prefix="${2:-src}"
  # 行ではなく未カバーの *関数*: covered な region が 1 つも無い関数は一度も
  # 呼ばれていないということで､こちらが動きようのある形だ — 「1 つの分岐が
  # 欠けている」ではなく「これを動かすものが何も無い」｡クロージャはそれぞれ
  # 独立した項目として現れるが､それが望みどおりだ: 何も走らせなかった
  # クロージャは､何も通らなかった分岐だ｡
  #
  # 位置だけを出す｡llvm は名前を v0 でマングルして報告するので
  # (`_RNvNtNtCs..._7twigpui4sync4auto4diff`)､その位置のソース行のほうが
  # どんなデマングルよりも読みやすい｡
  jq -r -n \
    --slurpfile cuts "$cutoffs_json" \
    --slurpfile cov "$cov_json" \
    --arg root "$repo_root/" \
    --arg prefix "$prefix" '
      ($cuts[0] | map({key: .file, value: .cut}) | from_entries) as $cut
      | $cov[0].data[0].functions[]
      | (.filenames[0] | strings | ltrimstr($root)) as $f
      | select($f != null and $cut[$f] != null)
      | select($f | startswith($prefix))
      | select(.regions[0][0] < $cut[$f])
      | select([.regions[] | select(.[4] != 0)] | length == 0)
      | "\($f):\(.regions[0][0])"
    ' | sort -u -t: -k1,1 -k2,2n >"$work_dir/gaps.txt"

  if [ ! -s "$work_dir/gaps.txt" ]; then
    printf 'No uncovered function under %s.\n' "$prefix"
    exit 0
  fi

  # ソースファイルはヒットごとではなく 1 回ずつ歩く: 数百の位置がある状況で
  # 1 行ごとの `sed -n` は遠回りだ｡
  # shellcheck disable=SC2046  # 単語分割が狙いだ: ファイル 1 つにつき引数 1 つ
  awk '
    NR == FNR {
      split($0, at, ":")
      want[at[1] SUBSEP at[2]] = 1
      next
    }
    (FILENAME SUBSEP FNR) in want {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      printf "%s:%s\t%s\n", FILENAME, FNR, line
    }
  ' "$work_dir/gaps.txt" $(cut -d: -f1 "$work_dir/gaps.txt" | sort -u)
  ;;
--json)
  measure >&2
  per_file_json
  ;;
"")
  measure >&2
  report=$(per_file_json)

  printf '## Coverage (implementation only)\n\n'

  printf '%s' "$report" | jq -r '
    ([.[] | .covered] | add) as $c
    | ([.[] | .total] | add) as $t
    | "**\((10000 * $c / $t | round) / 100)%** of \($t) regions, excluding everything under `#[cfg(test)]`.\n"
  '

  printf '\n| File | Regions | Covered | %% |\n'
  printf '| --- | ---: | ---: | ---: |\n'
  printf '%s' "$report" |
    jq -r '.[] | "| \(.file) | \(.total) | \(.covered) | \(.percent)% |"'

  # shellcheck disable=SC2016  # バッククォートは markdown であってサブシェルではない
  printf '\nRun `scripts/coverage.sh --gaps src/path.rs` for the functions '
  printf 'nothing calls. What is worth covering here — and what is not — is '
  # shellcheck disable=SC2016  # 同上
  printf 'in the `coverage-gaps` skill.\n'
  ;;
*)
  printf 'usage: %s [--gaps [path-prefix] | --json]\n' "$0" >&2
  exit 2
  ;;
esac
