#!/usr/bin/env bash
# Report test coverage of the implementation — not of the tests.
#
# `cargo llvm-cov` instruments everything it compiles, and this crate keeps
# its tests in the same file as the code under `#[cfg(test)]`. Those test
# functions run by definition, so they count as covered and pull the number
# up: 1077 of 1880 instrumented functions are test functions, and the
# headline figure reads 79% where the implementation alone is 54%
# (measured 2026-08-24). Reporting the raw number would be reporting how
# well the tests cover the tests.
#
# So every figure here is filtered to regions that start before the
# `#[cfg(test)]` line — the same boundary `code-metrics.sh` uses to split
# implementation lines from test lines. Two scripts, one definition of
# "implementation".
#
# Three modes:
#
# - no argument: print the markdown report (per-file table, then the
#   files with the least coverage). This is what CI appends to the step
#   summary, and what to read first locally.
# - `--gaps [path-prefix]`: print uncovered functions as
#   `file:line function` — one per line, sorted by file. This is the mode
#   an agent reads to decide where a test is missing. The optional prefix
#   narrows it to one file or directory.
# - `--json`: print the filtered per-file numbers as JSON, for anything
#   that wants to compute rather than read.
#
# Deliberately no threshold and no gate. The repo's rule (see
# `metrics-baseline.tsv`) is that a check failing from day one gets
# disabled instead of fixed, and a coverage floor has a second problem the
# file-size ratchet does not: this app's untestable surface is real and
# documented (#115 — menus, and everything past the `Scene`), so a feature
# PR that adds rendering code lowers the percentage through no fault of
# its own. The gaps list is the deliverable; the percentage is a map.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

work_dir="./tmp/coverage"
cov_json="$work_dir/llvm-cov.json"
cutoffs_json="$work_dir/cutoffs.json"

# `llvm-tools-preview` normally comes from rustup. A Homebrew toolchain has
# no rustup, so fall back to Homebrew's LLVM — but only when its version
# matches rustc's, since the profile format moves between LLVM releases and
# a mismatch fails in the middle of a report rather than at the start.
ensure_llvm_tools() {
  if [ -n "${LLVM_COV:-}" ] && [ -n "${LLVM_PROFDATA:-}" ]; then
    return
  fi

  local brew_llvm="/opt/homebrew/opt/llvm/bin"
  if [ ! -x "$brew_llvm/llvm-cov" ]; then
    # Nothing to fall back to. cargo-llvm-cov will say what is missing.
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

# Where each file stops being implementation. Mirrors
# `code-metrics.sh`'s `implementation_lines`, including how it tells "no
# test module" (grep exit 1) from a real read failure (anything else).
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
        # No test module: every line is implementation. Larger than any
        # real file, so nothing is filtered out.
        cut=999999
      fi
      [ "$first" -eq 1 ] || printf ','
      first=0
      printf '{"file":"%s","cut":%s}' "$file" "$cut"
    done < <(find src -name '*.rs' | sort)
    printf ']\n'
  } >"$cutoffs_json"
}

# Run the instrumented tests once, then report from that data. `report`
# re-reads the profile without rebuilding, so the three modes below cost
# one build between them rather than one each.
measure() {
  ensure_llvm_tools
  mkdir -p "$work_dir"

  if [ -z "${COVERAGE_REUSE:-}" ]; then
    cargo llvm-cov --locked --all-targets --no-report
  fi
  cargo llvm-cov report --json --output-path "$cov_json"
  write_cutoffs
}

# jq is fed both files and produces one row per source file. A function
# belongs to the implementation when its first region starts before the
# cutoff; `regions[i][4]` is the execution count, so a region is covered
# when that is not zero.
#
# `strings` guards the filename: llvm's JSON is machine-written, but this
# reads one field out of it by path and a non-string there would abort the
# whole filter rather than skip a row.
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
  # Uncovered *functions*, not lines: a function with no covered region at
  # all was never called, which is the actionable shape — "nothing
  # exercises this" rather than "one branch is missing". Closures appear
  # as their own entries, which is wanted: a closure nothing ran is a
  # branch nothing took.
  #
  # Locations only. llvm reports names v0-mangled
  # (`_RNvNtNtCs..._7twigpui4sync4auto4diff`), and the source line at that
  # location reads better than any demangling of it would.
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

  # Each source file is walked once rather than once per hit: with a few
  # hundred locations, a `sed -n` per line is the slow way round.
  # shellcheck disable=SC2046  # the word split is the point: one arg per file
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

  # shellcheck disable=SC2016  # the backticks are markdown, not a subshell
  printf '\nRun `scripts/coverage.sh --gaps src/path.rs` for the functions '
  printf 'nothing calls. What is worth covering here — and what is not — is '
  # shellcheck disable=SC2016  # same
  printf 'in the `coverage-gaps` skill.\n'
  ;;
*)
  printf 'usage: %s [--gaps [path-prefix] | --json]\n' "$0" >&2
  exit 2
  ;;
esac
