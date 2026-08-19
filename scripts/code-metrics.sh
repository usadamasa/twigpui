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
# Reports only. It never exits non-zero on a metric, because the existing
# code is already over any threshold worth setting and a check that fails
# from day one gets disabled rather than fixed. Thresholds come after the
# numbers, which is #48's own instruction.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

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

  # grep exits 1 for "no match" and 2 for a real failure. Only the first is
  # an ordinary outcome here — a file with no test module — so the two are
  # told apart rather than both swallowed with `|| true`, which would hide
  # an unreadable file as "no tests".
  if match=$(grep -n -m1 '^#\[cfg(test)\]' "$file"); then
    test_start=${match%%:*}
    impl_lines=$((test_start - 1))
    test_lines=$((total - impl_lines))
  else
    status=$?
    if [ "$status" -ne 1 ]; then
      printf 'error: could not read %s (grep exit %s)\n' "$file" "$status" >&2
      exit 1
    fi
    impl_lines="$total"
    test_lines=0
  fi

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
