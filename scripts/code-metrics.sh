#!/usr/bin/env bash
# Report structural metrics for this crate (#48).
#
# #47 asks "is this expression correct". This asks "is the structure coming
# apart" — file sizes, function lengths, and cognitive complexity.
#
# Deliberately built from bash, awk and clippy alone. Every candidate tool
# in #48 (rust-code-analysis, cargo-modules, jscpd, cargo-bloat) needs an
# install step on every CI run, and #46 is an open issue about build time.
# A report nobody waits for is worth more than a better report that makes
# every push slower. When one of those tools earns its install time, this
# script is the thing it replaces.
#
# Two modes:
#
# - no argument: print the report. Never fails on a metric.
# - `--check`: compare implementation line counts against
#   `metrics-baseline.tsv` and exit non-zero if any file is over its
#   ceiling, or is missing from the baseline entirely. Prints nothing else.
#
# The split is deliberate. Function length is already enforced by
# `clippy::too_many_lines` (denied via `pedantic`, #47) and cognitive
# complexity has no hits to gate on, so file size is the one metric with
# both a real problem and no existing check — and it is gated by a ratchet
# against today's numbers rather than an ideal nobody can reach from here.
# See `metrics-baseline.tsv` for why that is the shape.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

baseline_file="metrics-baseline.tsv"

# Implementation lines in $1 — everything before the `#[cfg(test)]` line,
# or the whole file when there is none. This crate keeps its tests in the
# same file, and a file that is 60% tests is not the same problem as one
# that is 60% rendering code.
implementation_lines() {
  local file="$1" total match
  total=$(wc -l <"$file" | tr -d ' ')

  # grep exits 1 for "no match" and 2 for a real failure. Only the first is
  # an ordinary outcome here — a file with no test module — so the two are
  # told apart rather than both swallowed with `|| true`, which would hide
  # an unreadable file as "no tests".
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

    # awk on the tab-separated field rather than grep on the line: an exact
    # field match cannot let `src/like.rs` be satisfied by `src/liked.rs`,
    # and BSD grep (which is what macOS ships) has no `-P` for `\t`. awk
    # exits non-zero when the path is absent, which is the signal here.
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

# --- file sizes -------------------------------------------------------------
#
# Implementation lines, not total: this crate keeps its tests in the same
# file, and a file that is 60% tests is not the same problem as one that is
# 60% rendering code. The split is the `#[cfg(test)]` line, which rustfmt
# always puts at column 0 in this codebase.

printf '## File sizes\n\n'
printf '| File | Total | Implementation | Tests |\n'
printf '| --- | ---: | ---: | ---: |\n'

while IFS= read -r file; do
  total=$(wc -l <"$file" | tr -d ' ')
  impl_lines=$(implementation_lines "$file")
  test_lines=$((total - impl_lines))
  printf '| %s | %s | %s | %s |\n' "$file" "$total" "$impl_lines" "$test_lines"
done < <(find src -name '*.rs' | sort)

# --- longest functions ------------------------------------------------------
#
# rustfmt puts a function's closing brace at exactly the indentation of its
# `fn`, which is what makes this measurable without a parser. Approximate by
# construction: it counts lines, so a long match arm and a long chain of
# gpui builder calls weigh the same. That is the right approximation here —
# `ui.rs`'s length problem is builder chains.

printf '\n## Longest functions (implementation only)\n\n'
printf '| Lines | Function | File |\n'
printf '| ---: | --- | --- |\n'

# shellcheck disable=SC2016  # `$0`/`$1` below are awk's fields, not shell's
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
  # `sed -n '1,15p'`, not `head`: `head` closes the pipe early, which under
  # `set -o pipefail` makes `sort` fail with SIGPIPE and takes the whole
  # script down before it prints anything else.
  sed -n '1,15p' |
  awk -F'\t' '{ printf "| %s | `%s` | %s |\n", $1, $2, $3 }'

# --- cognitive complexity ---------------------------------------------------
#
# A nursery lint, so it is reported here rather than denied in Cargo.toml:
# #47's rule is that a lint producing `#[allow]`s is not adopted, and this
# one needs the numbers looked at before that can be decided.

printf '\n## Cognitive complexity (clippy::cognitive_complexity)\n\n'

# `touch` because clippy caches per crate: without it a second run in the
# same CI job reports nothing and the section reads as clean.
touch src/main.rs
if ! complexity=$(cargo clippy --all-targets --message-format=json --quiet -- \
  -W clippy::cognitive_complexity 2>&1); then
  # shellcheck disable=SC2016  # the format string is printf's, expanded below
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
