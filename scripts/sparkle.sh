#!/bin/bash
# Fetch Sparkle into target/sparkle/: its framework, which scripts/app.sh
# copies into the app, and its tools, of which scripts/sign-release.sh runs
# sign_update. It's the release Sparkle publishes for Swift packages, at the
# version below, checked against its checksum, and never built. Run again, it
# does nothing once that version is there.
#
#   scripts/sparkle.sh
set -euo pipefail
cd "$(dirname "$0")/.."

# To update Sparkle: its version, and the checksum of that release's
# Sparkle-for-Swift-Package-Manager.zip, as its Package.swift gives it.
version=2.10.0
checksum=17e28312b8e18ab7cdbbe09a6fb28cc55a5479ec6c371dbc07cdecd2a14fd959

out=target/sparkle
if [ "$(cat "$out/VERSION" 2>/dev/null)" = "$version" ]; then
  exit 0
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -sSfL -o "$work/sparkle.zip" \
  "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-for-Swift-Package-Manager.zip"
echo "$checksum  $work/sparkle.zip" | shasum -a 256 --check --status || {
  echo "sparkle.sh: Sparkle $version's download doesn't match its checksum" >&2
  exit 1
}
ditto -x -k "$work/sparkle.zip" "$work/sparkle"
rm -rf "$out"
mkdir -p "$out"
ditto "$work/sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework" "$out/Sparkle.framework"
ditto "$work/sparkle/bin" "$out/bin"
echo "$version" > "$out/VERSION"
echo "Fetched Sparkle $version into $out"
