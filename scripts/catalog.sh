#!/usr/bin/env bash
# Refresh what ships with the app from models.dev: the catalog of models, with
# their prices, dates and names, and every provider's logo.
#
#   scripts/catalog.sh
#
# The catalog goes to crates/engine/data/catalog.json, as `turnscope catalog`
# writes it; the logos to macos/Logos/providers/, one SVG each, named by
# the provider's id. All of it is fetched and checked in a directory of its
# own first, and none of it is installed unless all of it passes, so a
# refresh that fails part way leaves the checkout as it was.
#
# The logos are those of the providers models.dev lists now, as the catalog
# is: a provider it no longer lists takes its logo with it, which git shows
# before anything is committed. A provider models.dev has no logo for, which
# it answers with a placeholder of its own, or one whose logo is drawn from a
# picture rather than shapes, which can't be drawn in one color as the app
# draws logos, is left out, and an API key's account with it shows a key.
# The refresh stops instead if that would take away a logo the app shows
# for an agent or a subscription.
set -euo pipefail
cd "$(dirname "$0")/.."

logos=macos/Logos/providers

# As the app asks models.dev (crates/engine/src/net.rs): macOS's curl, without
# the person's ~/.curlrc (-q, which must come first), over HTTPS alone even
# when redirected, bounded in time and size, and failing on an HTTP error.
fetch() {
  local seconds=$1 bytes=$2 to=$3 url=$4
  /usr/bin/curl -q --silent --show-error --fail --compressed \
    --proto =https --location --proto-redir =https \
    --max-time "$seconds" --max-filesize "$bytes" --user-agent Turnscope \
    --output "$to" "$url"
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/logos"

fetch 60 67108864 "$work/api.json" https://models.dev/api.json
# The app's own reading of it, which refuses one that isn't whole.
cargo run -q --release --locked -p turnscope -- catalog "$work/api.json" "$work/catalog.json"

# What models.dev answers for a provider it has no logo for: a placeholder,
# not an error (2026-09-27). An error, for this or any logo, stops the
# refresh, since it can't tell a logo that is gone from one that failed to
# come.
fetch 20 1048576 "$work/placeholder.svg" https://models.dev/logos/turnscope-no-such-provider.svg

pictures=()
placeholders=()
unnamable=()
ids=$(/usr/bin/jq -r 'keys[]' "$work/api.json")
while IFS= read -r id; do
  # An id becomes a file's name, so it is one that can be nothing else.
  if ! [[ $id =~ ^[a-z0-9][a-z0-9._-]*$ ]]; then
    unnamable+=("$id")
    continue
  fi
  logo="$work/logos/$id.svg"
  fetch 20 1048576 "$logo" "https://models.dev/logos/$id.svg"
  if cmp -s "$logo" "$work/placeholder.svg"; then
    placeholders+=("$id")
    rm "$logo"
  elif ! grep -q '<svg' "$logo"; then
    echo "catalog.sh: models.dev's logo for $id is not an SVG; nothing was changed." >&2
    exit 1
  elif grep -q '<image' "$logo"; then
    pictures+=("$id")
    rm "$logo"
  fi
done <<<"$ids"

# The logos the app shows for a subscription or an agent: each
# subscription's provider (Subscription::providers in the engine), and those
# the app names for agents (macos/Turnscope/Logos.swift).
for id in anthropic openai xai opencode opencode-go; do
  if [ ! -f "$work/logos/$id.svg" ]; then
    echo "catalog.sh: models.dev no longer draws $id's logo, which the app shows for an agent or a subscription; nothing was changed." >&2
    exit 1
  fi
done

# Everything passed: install it.
mv "$work/catalog.json" crates/engine/data/catalog.json
mkdir -p "$logos"
removed=()
for old in "$logos"/*.svg; do
  [ -e "$old" ] || continue
  name=$(basename "$old")
  if [ ! -e "$work/logos/$name" ]; then
    removed+=("${name%.svg}")
    rm "$old"
  fi
done
fetched=0
for new in "$work/logos"/*.svg; do
  [ -e "$new" ] || continue
  mv "$new" "$logos/"
  fetched=$((fetched + 1))
done

echo "Installed the catalog and $fetched logos."
if ((${#placeholders[@]})); then
  echo "models.dev has no logo for: ${placeholders[*]}"
fi
if ((${#pictures[@]})); then
  echo "Drawn from a picture, so left out: ${pictures[*]}"
fi
if ((${#unnamable[@]})); then
  echo "Ids that can't name a file, so left out: ${unnamable[*]}"
fi
if ((${#removed[@]})); then
  echo "Removed, no longer listed or drawn: ${removed[*]}"
fi
