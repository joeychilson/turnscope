#!/usr/bin/env bash
# Check that a built Turnscope.app runs, as its bundle holds it:
#
#   scripts/smoke.sh [Turnscope.app]   (default: target/dist/Turnscope.app)
#
# Its command line reads a home with no history into a new ledger (`doctor`),
# writes the app's feed in the version the contract holds (`watch`,
# contract/feed.json), and answers an MCP client's handshake and its request
# for the tools, with the version the bundle claims. Only commands run, never
# the menu bar app, and only over directories made for the run: HOME is one
# of them too, so nothing of the person's or the runner's own is read or
# written. Run on Apple silicon, a universal app is checked for that
# architecture alone.
set -euo pipefail

root="$(dirname "$0")/.."
app=${1:-"$root/target/dist/Turnscope.app"}
contract=$(jq -e '.version' "$root/contract/feed.json")
executable="$app/Contents/Helpers/turnscope"
for needed in "$executable" "$app/Contents/MacOS/Turnscope"; do
  if [ ! -x "$needed" ]; then
    echo "smoke.sh: no executable at $needed" >&2
    exit 1
  fi
done
version=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/home" "$work/data"
turnscope() {
  HOME="$work/home" "$executable" "$@" --home "$work/home" --data "$work/data"
}

if ! report=$(turnscope doctor 2>&1); then
  printf 'smoke.sh: doctor failed:\n%s\n' "$report" >&2
  exit 1
fi
if [ ! -s "$work/data/ledger.sqlite" ]; then
  printf 'smoke.sh: doctor kept no ledger. It said:\n%s\n' "$report" >&2
  exit 1
fi

# With its input closed at once, as when the app quits, it writes one feed
# and stops.
if ! feed=$(turnscope watch </dev/null) ||
  ! jq -e --argjson version "$contract" \
    '.feed.version == $version and (.feed.accounts | type) == "array"' <<<"$feed" >/dev/null; then
  printf 'smoke.sh: watch wrote no feed the app reads:\n%s\n' "$(cut -c 1-300 <<<"$feed")" >&2
  exit 1
fi

# The handshake a client opens with, then the tools it asks for. The
# notification between them needs no answer, so two come back.
requests='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
if ! answers=$(turnscope mcp <<<"$requests"); then
  echo "smoke.sh: the MCP server failed" >&2
  exit 1
fi
if ! tools=$(jq -e -s -r --arg version "$version" '
  if length == 2
    and all(.[]; .jsonrpc == "2.0" and has("result"))
    and .[0].id == 1 and .[1].id == 2
    and .[0].result.serverInfo.name == "turnscope"
    and .[0].result.serverInfo.version == $version
    and (.[0].result.protocolVersion | type) == "string"
    and (.[1].result.tools | length) > 0
    and all(.[1].result.tools[]; (.name | type) == "string")
  then [.[1].result.tools[].name] | join(", ")
  else false end' <<<"$answers"); then
  printf 'smoke.sh: the MCP server of Turnscope %s answered, cut short:\n%s\n' \
    "$version" "$(cut -c 1-300 <<<"$answers")" >&2
  exit 1
fi

echo "Turnscope $version ($(lipo -archs "$executable")) ran from $app:"
echo "  doctor: $(head -n 1 <<<"$report")"
echo "  watch: a feed of $(jq '.feed.accounts | length' <<<"$feed") accounts"
echo "  mcp: $tools"
