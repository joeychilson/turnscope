#!/bin/bash
# Check that a built Turnscope.app works as packaged:
#
#   scripts/check-app.sh [Turnscope.app]   (default: target/dist/Turnscope.app)
#
# Its `turnscope`, off the network, creates a database from an empty home
# (`doctor`), answers a hello with a status (`serve`), and answers an MCP
# handshake and tool list with the bundle's version. Then the menu bar app,
# with no engine, draws its panel from the contract's example status and
# quits. All of it runs for each architecture the app was built for that
# this Mac can run: on Apple silicon, Intel's runs under Rosetta, if Rosetta
# is installed. Everything runs in folders made for the run, HOME included,
# so nothing of yours or the CI runner's is read or written.
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
# `$serve` is the subshell that runs the `turnscope` function in the
# background, so serve is its child.
stop_serve() {
  if [ -n "$serve" ]; then
    pkill -P "$serve" 2>/dev/null || true
    wait "$serve" 2>/dev/null || true
    serve=
  fi
}
trap 'stop_serve; rm -rf "$work"' EXIT
mkdir "$work/home"
# `turnscope` as built for `$slice`, with that architecture's own data.
turnscope() {
  HOME="$work/home" TURNSCOPE_OFFLINE=1 arch "-$slice" "$executable" \
    --home "$work/home" --data "$work/$slice" "$@"
}

example="$root/contract/status.notification.json"
slices=$(lipo -archs "$executable")
checked=0
echo "Turnscope $version from $app:"
for slice in $slices; do
  if ! arch "-$slice" /usr/bin/true 2>/dev/null; then
    echo "  $slice: not checked, as this Mac can't run it"
    continue
  fi
  mkdir "$work/$slice"

  if ! report=$(turnscope doctor 2>"$work/doctor.err"); then
    printf 'check-app.sh: doctor (%s) failed:\n%s\n' "$slice" "$(cat "$work/doctor.err")" >&2
    exit 1
  fi
  if [ ! -s "$work/$slice/turnscope.sqlite" ]; then
    printf 'check-app.sh: doctor (%s) kept no store. It said:\n%s\n' "$slice" "$report" >&2
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
    printf 'check-app.sh: serve (%s) answered no hello:\n%s\n' "$slice" "$(cut -c 1-300 <<<"${answer:-}")" >&2
    exit 1
  fi
  stop_serve

  # The handshake an MCP client opens with, then the tools it asks for. The
  # notification between them needs no answer, so two come back.
  requests='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"check-app","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  if ! answers=$(turnscope mcp <<<"$requests"); then
    echo "check-app.sh: the MCP server ($slice) failed" >&2
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
    printf 'check-app.sh: the MCP server (%s) of Turnscope %s answered, cut short:\n%s\n' \
      "$slice" "$version" "$(cut -c 1-300 <<<"$answers")" >&2
    exit 1
  fi

  # The app itself, off-screen and with no engine: it draws the panel from
  # a status file, light and dark, as PNGs, and quits.
  panel="$work/panel-$slice"
  if ! HOME="$work/home" arch "-$slice" "$app/Contents/MacOS/Turnscope" \
    --snapshot "$panel" --fixture "$example" 2>"$work/app.err"; then
    printf 'check-app.sh: the app (%s) failed to draw its panel:\n%s\n' "$slice" "$(cat "$work/app.err")" >&2
    exit 1
  fi
  for look in light dark; do
    if [ ! -s "$panel/panel-$look.png" ]; then
      printf 'check-app.sh: the app (%s) drew no %s panel. It said:\n%s\n' \
        "$slice" "$look" "$(cat "$work/app.err")" >&2
      exit 1
    fi
  done

  echo "  $slice doctor: $(head -n 1 <<<"$report")"
  echo "  $slice serve: protocol $protocol, a hello answered with a status"
  echo "  $slice mcp: $tools"
  echo "  $slice app: drew its panel, light and dark, from the contract's example status"
  checked=$((checked + 1))
done
if [ "$checked" -eq 0 ]; then
  echo "check-app.sh: this Mac can run none of its architectures ($slices)" >&2
  exit 1
fi
