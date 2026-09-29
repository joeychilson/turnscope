#!/usr/bin/env bash
# Print a release's notes: its section of CHANGELOG.md, without the heading.
#
#   scripts/notes.sh X.Y.Z
#
# The release workflow publishes them with the release, whose page is what
# Sparkle shows a copy offered the update, so a version without a section, or
# with an empty one, stops the release.
set -euo pipefail
cd "$(dirname "$0")/.."

version=${1:?usage: scripts/notes.sh X.Y.Z}
notes=$(awk -v version="$version" '
  /^## / {
    if (inside) exit
    heading = $2
    gsub(/[][]/, "", heading)
    inside = (heading == version)
    next
  }
  inside && (started || NF) { started = 1; print }
' CHANGELOG.md)

if [ -z "${notes//[[:space:]]/}" ]; then
  echo "notes.sh: CHANGELOG.md has no notes under \"## [$version]\"" >&2
  exit 1
fi
printf '%s\n' "$notes"
