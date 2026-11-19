#!/bin/bash
# Redraw the README's screenshots, light and dark, from the made-up example
# status in crates/turnscope/tests/fixtures/status.json, so they never show
# anyone's real accounts and change only when the app does:
#
#   panel   an account in use, opened, above the accounts used this week
#   idle    nothing in use, so the accounts used this week come first
#   agents  Settings → Agents
#
#   scripts/screenshots.sh
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --locked -p turnscope-macos
app=${CARGO_TARGET_DIR:-target}/debug/turnscope-app
example=crates/turnscope/tests/fixtures/status.json
drawn=$(mktemp -d)
trap 'rm -rf "$drawn"' EXIT

# The example a few hours later, with nothing in use: Claude Max's 5-hour
# window has reset, so its weekly limit is the one shown, and the accounts
# are in the order the engine lists them, the most left first.
jq '.agents |= map(.inUse = false)
  | .accounts |= map(.inUse = false | .usedBy = [])
  | .accounts |= map(if .id == "anthropic:acct-1:org-1" then
      .deciding = "seven_day"
      | .limits |= map(if .key == "five_hour" then
          .leftPercent = 100 | .outlook = {kind: "lasts", leftAtReset: null} | .work = null
        else . end)
    else . end)
  | .accounts |= sort_by(.id as $id
      | ["openai:acct-2", "anthropic:acct-1:org-1", "openrouter:api"] | index($id) // 3)' \
  "$example" > "$drawn/idle.json"

# Draw `$1` from the status in `$2`, opening what `$3` names, if anything.
shot() {
  local name=$1 fixture=$2 opening=${3:-}
  "$app" --snapshot "$drawn/$name" --fixture "$fixture" ${opening:+--open "$opening"}
  for look in light dark; do
    mv "$drawn/$name/panel-$look.png" ".github/screenshots/$name-$look.png"
  done
}
shot panel "$example" account
shot idle "$drawn/idle.json"
shot agents "$example" agents
echo "Drew the README's screenshots again in .github/screenshots"
