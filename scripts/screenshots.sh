#!/bin/bash
# Redraw the README's panel screenshots, light and dark, from the made-up
# example status in crates/turnscope/tests/fixtures/status.json, so they
# never show anyone's real accounts and change only when the app does.
#
#   scripts/screenshots.sh
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/app.sh
app=target/debug/Turnscope.app/Contents/MacOS/Turnscope
example=crates/turnscope/tests/fixtures/status.json
drawn=$(mktemp -d)
trap 'rm -rf "$drawn"' EXIT

TURNSCOPE_OFFLINE=1 "$app" --snapshot "$drawn/panel" --fixture "$example" --open account
for look in light dark; do
  mv "$drawn/panel/panel-$look.png" ".github/screenshots/panel-$look.png"
done
echo "Drew the README's screenshots again in .github/screenshots"
