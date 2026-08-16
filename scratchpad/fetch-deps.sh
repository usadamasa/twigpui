#!/usr/bin/env bash
# Populate a project-local CARGO_HOME despite the sandbox's filename-based
# write denies.
#
# The sandbox refuses writes to files named .gitmodules or settings.json
# anywhere on disk. Some crate tarballs ship those, so `cargo fetch` dies
# mid-unpack. Nothing in a Rust build reads them, so we let cargo download the
# .crate archives and unpack them ourselves with those paths excluded, then
# write the .cargo-ok marker cargo looks for.
set -euo pipefail

PROJECT=/Users/usadamasa/src/github.com/usadamasa/twigpui
export CARGO_HOME="${PROJECT}/.cargo-home"
TARGET=aarch64-apple-darwin
MAX_ROUNDS=60

mkdir -p "${CARGO_HOME}"

# Paths the sandbox will not let us create. None affect compilation.
EXCLUDES=(
  --exclude=.gitmodules
  --exclude=settings.json
)

unpacked_this_round=0

unpack_pending() {
  local cache_root src_root archive name dest tar_out rc
  unpacked_this_round=0

  for cache_root in "${CARGO_HOME}"/registry/cache/*/; do
    [ -d "${cache_root}" ] || continue
    src_root="${CARGO_HOME}/registry/src/$(basename "${cache_root}")"
    mkdir -p "${src_root}"

    for archive in "${cache_root}"*.crate; do
      [ -f "${archive}" ] || continue
      name=$(basename "${archive}" .crate)
      dest="${src_root}/${name}"
      [ -f "${dest}/.cargo-ok" ] && continue

      rc=0
      tar_out=$(tar xzf "${archive}" -C "${src_root}" "${EXCLUDES[@]}" 2>&1) || rc=$?
      if [ "${rc}" -ne 0 ]; then
        printf 'unpack failed: %s\n%s\n' "${name}" "${tar_out}" >&2
        continue
      fi
      if [ ! -d "${dest}" ]; then
        printf 'unpack produced no directory: %s\n' "${name}" >&2
        continue
      fi
      printf '{"v":1}' >"${dest}/.cargo-ok"
      unpacked_this_round=$((unpacked_this_round + 1))
    done
  done
}

for round in $(seq 1 "${MAX_ROUNDS}"); do
  if cargo fetch --target "${TARGET}" >/dev/null 2>&1; then
    printf 'fetch complete after %s round(s)\n' "${round}"
    exit 0
  fi

  unpack_pending
  printf 'round %s: unpacked %s crate(s)\n' "${round}" "${unpacked_this_round}"

  if [ "${unpacked_this_round}" -eq 0 ]; then
    printf 'no progress; surfacing the real cargo error\n' >&2
    cargo fetch --target "${TARGET}" >&2
    exit 1
  fi
done

printf 'still incomplete after %s rounds\n' "${MAX_ROUNDS}" >&2
exit 1
