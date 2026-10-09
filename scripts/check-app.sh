#!/bin/bash
# Check that a built Turnscope.app works as packaged:
#
#   scripts/check-app.sh [Turnscope.app]   (default: target/dist/Turnscope.app)
#
# Its `turnscope` creates a database from an empty home (`doctor`), answers
# a hello with a status (`serve`), and answers an MCP handshake and tool list
# with the bundle's version. Only the command line runs, never the menu bar
# app, and only in folders made for the run, HOME included, so nothing of
# yours or the CI runner's is read or written. On Apple silicon, a universal
# app is checked for that architecture only.
set -euo pipefail

root="$(dirname "$0")/.."
app=${1:-"$root/target/dist/Turnscope.app"}
protocol=$(jq -e '.params.protocol' "$root/contract/hello.request.json")
executable="$app/Contents/Helpers/turnscope"
for needed in "$executable" "$app/Contents/MacOS/Turnscope"; do
  if [ ! -x "$needed" ]; then
    echo "check-app.sh: no executable at $needed" >&2
    exit 1
  fi
done
version=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")

work=$(mktemp -d)
serve=
cleanup() {
  if [ -n "$serve" ]; then
    kill "$serve" 2>/dev/null || true
    wait "$serve" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
mkdir "$work/home" "$work/data"
turnscope() {
  HOME="$work/home" "$executable" --home "$work/home" --data "$work/data" "$@"
}

if ! report=$(turnscope doctor 2>"$work/doctor.err"); then
  printf 'check-app.sh: doctor failed:\n%s\n' "$(cat "$work/doctor.err")" >&2
  exit 1
fi
if [ ! -s "$work/data/turnscope.sqlite" ]; then
  printf 'check-app.sh: doctor kept no store. It said:\n%s\n' "$report" >&2
  exit 1
fi

# A client's hello, in the contract's protocol, is answered with a status.
socket=$(turnscope serve --socket)
turnscope serve 2>/dev/null &
serve=$!
for _ in $(seq 50); do
  [ -S "$socket" ] && break
  sleep 0.1
done
hello='{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":'"$protocol"',"client":"check-app"}}'
if ! answer=$(printf '%s\n' "$hello" | nc -U -w 2 "$socket" | head -n 1) ||
  ! jq -e '.result.status.accounts | type == "array"' <<<"$answer" >/dev/null; then
  printf 'check-app.sh: serve answered no hello:\n%s\n' "$(cut -c 1-300 <<<"${answer:-}")" >&2
  exit 1
fi

# The handshake an MCP client opens with, then the tools it asks for. The
# notification between them needs no answer, so two come back.
requests='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"check-app","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
if ! answers=$(turnscope mcp <<<"$requests"); then
  echo "check-app.sh: the MCP server failed" >&2
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
  printf 'check-app.sh: the MCP server of Turnscope %s answered, cut short:\n%s\n' \
    "$version" "$(cut -c 1-300 <<<"$answers")" >&2
  exit 1
fi

echo "Turnscope $version ($(lipo -archs "$executable")) ran from $app:"
echo "  doctor: $(head -n 1 <<<"$report")"
echo "  serve: protocol $protocol, a hello answered with a status"
echo "  mcp: $tools"
