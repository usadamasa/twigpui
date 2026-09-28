#!/usr/bin/env bash
# X API のクレジット消費を usage.json とログから集計し､static な HTML に描く｡
#
#   scripts/credit-report.sh --purchase '2026-09-19 13:42' --amount 5
#
# 描けるのは usage.json が持っている UTC 日の 1 日分だけ｡課金済み post id
# (dedup.posts.ids) は UTC 0 時に消えるので､残したい日は --state-dir に
# コピーを取ってから渡す｡時刻は post id の snowflake から起こした作成時刻で
# 近似する｡手順と読み方は credit-report スキルにある｡
#
# 単価は .claude/skills/x-api-budget/reference/pricing.md､endpoint と種別の対応は
# src/usage/kind.rs の写し｡どちらかが変わったら下の jq の表も直す｡
set -euo pipefail

usage() {
  printf '%s\n' "usage: $0 [--state-dir DIR] [--purchase 'YYYY-MM-DD HH:MM'] [--amount USD] [--out FILE]" >&2
  exit 2
}

state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/twigpui"
out="./tmp/credit-report.html"
purchase=""
amount=""
while (($# > 0)); do
  case "$1" in
    --state-dir) state_dir="${2:?}"; shift 2 ;;
    --purchase) purchase="${2:?}"; shift 2 ;;
    --amount) amount="${2:?}"; shift 2 ;;
    --out) out="${2:?}"; shift 2 ;;
    *) usage ;;
  esac
done

if [[ "$state_dir" != /* ]]; then
  printf '%s\n' "--state-dir は絶対パスで渡す: $state_dir" >&2
  exit 2
fi
command -v jq >/dev/null || { printf '%s\n' "jq が無い" >&2; exit 1; }

template="$(cd "$(dirname "$0")" && pwd)/credit-report.html"
usage_json="$state_dir/usage.json"
[[ -f "$usage_json" ]] || { printf '%s\n' "usage.json が無い: $usage_json" >&2; exit 1; }

# ログは state の logs/ か､コピーを取ったディレクトリの直下にある｡古い世代が先
logs=()
for f in "$state_dir/logs/twigpui.log.1" "$state_dir/logs/twigpui.log" "$state_dir/twigpui.log.1" "$state_dir/twigpui.log"; do
  [[ -f "$f" ]] && logs+=("$f")
done
((${#logs[@]} > 0)) || { printf '%s\n' "ログが無い: $state_dir" >&2; exit 1; }

# 購入時刻はローカル時刻で受けて unix 秒へ (macOS の date)
purchase_epoch="null"
if [[ -n "$purchase" ]]; then
  # 秒まで渡さないと date -j が現在時刻の秒で埋める
  if ! purchase_epoch="$(date -j -f '%Y-%m-%d %H:%M:%S' "$purchase:00" '+%s')"; then
    printf '%s\n' "--purchase は 'YYYY-MM-DD HH:MM' で渡す: $purchase" >&2
    exit 2
  fi
fi

if ! html="$(cat "${logs[@]}" | jq -R -s -r \
  --slurpfile usage "$usage_json" --rawfile tpl "$template" \
  --argjson purchase "$purchase_epoch" --arg amount "$amount" '
  {
    timeline: ["posts", 0.005], home_timeline: ["posts", 0.005], list_timeline: ["posts", 0.005],
    tweet_by_id: ["posts", 0.005],
    user_lookup: ["users", 0.010], list_members: ["users", 0.010],
    me: ["owned", 0.001], owned_lists: ["owned", 0.001], following: ["owned", 0.001],
    create_post: ["write", 0.015], add_list_member: ["write", 0.010]
  } as $price
  | $usage[0] as $u
  | ($u.dedup.posts // error("usage.json に課金済み Posts の記録が無い")) as $posts
  | $posts.epoch_day as $day
  | ($day * 86400 | strftime("%Y-%m-%d")) as $date
  | [split("\n")[] | select(startswith($date)) | capture("^(?<t>\\S+) (?<level>\\S+) (?<msg>.*)$")] as $lines
  | [$u.endpoints | to_entries[] | select(.value.today_epoch_day == $day and .value.today > 0)
      | .key as $k
      | { name: $k, count: .value.today, kind: ($price[$k][0] // "write"), usd: $price[$k][1],
          failed: ([$lines[] | select(.msg | contains("\($k): HTTP 4"))] | length) }] as $endpoints
  | ([$endpoints[] | select(.kind == "posts") | .count] | add // 0) as $counted
  | if $counted != ($posts.ids | length)
    then error("Posts の件数が合わない: endpoints \($counted) / dedup \($posts.ids | length)") else . end
  | { date: $date, purchase: $purchase, amount: (if $amount == "" then null else ($amount | tonumber) end),
      post_ids: $posts.ids, endpoints: $endpoints,
      depleted: [$lines[] | select(.msg | contains("HTTP 402")) | .t] } as $data
  | $tpl | split("__DATA__") | join($data | tojson)
')"; then
  printf '%s\n' "集計に失敗した" >&2
  exit 1
fi

mkdir -p "$(dirname "$out")"
printf '%s\n' "$html" >"$out"
printf '%s\n' "$out"
