#!/usr/bin/env bash
# Draw the README's screenshots again, in light and dark, from the app as it
# draws the contract's feed (contract/feed.json), so they show made-up
# accounts, never anyone's own, and change only when the app does.
#
#   scripts/screenshots.sh
set -euo pipefail
cd "$(dirname "$0")/.."

scripts/build.sh
target/xcode/Build/Products/Debug/Turnscope.app/Contents/MacOS/Turnscope \
  --snapshot .github/screenshots --fixture contract/feed.json
echo "Drew the README's screenshots again in .github/screenshots"
