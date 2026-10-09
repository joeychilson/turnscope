#!/bin/bash
# Print a version's release notes: its section of CHANGELOG.md, without the
# heading.
#
#   scripts/release-notes.sh X.Y.Z
#
# The release workflow publishes them on the GitHub release, which is also
# the page Sparkle shows when it offers the update. A version with no section,
# or an empty one, stops the release.
set -euo pipefail
cd "$(dirname "$0")/.."

version=${1:?usage: scripts/release-notes.sh X.Y.Z}
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
  echo "release-notes.sh: CHANGELOG.md has no notes under \"## $version\"" >&2
  exit 1
fi
printf '%s\n' "$notes"
