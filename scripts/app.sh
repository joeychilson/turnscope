#!/bin/bash
# Build Turnscope.app:
#
#   scripts/app.sh            for this Mac, to try: target/debug/Turnscope.app,
#                             signed ad hoc
#   scripts/app.sh --release  for Apple silicon and Intel, unsigned:
#                             target/dist/Turnscope.app, which
#                             scripts/sign-release.sh signs
#
#   SPARKLE_PUBLIC_KEY  the public half of the key updates are signed with,
#                       which the app carries to check them; without it the
#                       app can't update itself
#
# This is the one place the bundle's layout is written:
#
#   Contents/Info.plist          crates/turnscope-macos/assets/Info.plist, with
#                                the version and SPARKLE_PUBLIC_KEY
#   Contents/MacOS/Turnscope     the menu bar app
#   Contents/Helpers/turnscope   the command line, which connected agents run:
#                                it stays here, as 0.2 put it, or they'd lose it
#   Contents/Frameworks/         Sparkle, which the app loads to update itself
#   Contents/Resources/          the icon, compiled from assets/Turnscope.icon
set -euo pipefail
cd "$(dirname "$0")/.."

# A private key in the environment isn't handed to what the build runs.
unset SPARKLE_PRIVATE_KEY

assets=crates/turnscope-macos/assets
# `cargo pkgid` ends `#0.3.0` for a path package and `@0.3.0` for others.
version=$(cargo pkgid -p turnscope | sed 's/.*[#@]//')
if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "app.sh: Cargo.toml's version, $version, isn't X.Y.Z" >&2
  exit 2
fi
minimum=$(plutil -extract LSMinimumSystemVersion raw "$assets/Info.plist")
export MACOSX_DEPLOYMENT_TARGET=$minimum

built=$(mktemp -d)
trap 'rm -rf "$built"' EXIT
case "${1:-}" in
  "")
    app=target/debug/Turnscope.app
    cargo build --locked -p turnscope -p turnscope-macos
    cp "${CARGO_TARGET_DIR:-target}/debug/turnscope" "${CARGO_TARGET_DIR:-target}/debug/turnscope-app" "$built"
    ;;
  --release)
    app=target/dist/Turnscope.app
    # Each binary for both architectures, joined into one file.
    for target in aarch64-apple-darwin x86_64-apple-darwin; do
      cargo build --locked --release -p turnscope -p turnscope-macos --target "$target"
    done
    for binary in turnscope turnscope-app; do
      lipo -create -output "$built/$binary" \
        "${CARGO_TARGET_DIR:-target}"/{aarch64,x86_64}-apple-darwin/release/"$binary"
    done
    ;;
  *)
    echo "usage: scripts/app.sh [--release]" >&2
    exit 2
    ;;
esac

scripts/sparkle.sh
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Frameworks" "$app/Contents/Resources"
plist="$app/Contents/Info.plist"
cp "$assets/Info.plist" "$plist"
plutil -replace CFBundleShortVersionString -string "$version" "$plist"
plutil -replace CFBundleVersion -string "$version" "$plist"
plutil -replace SUPublicEDKey -string "${SPARKLE_PUBLIC_KEY:-}" "$plist"
cp "$built/turnscope-app" "$app/Contents/MacOS/Turnscope"
cp "$built/turnscope" "$app/Contents/Helpers/turnscope"
ditto target/sparkle/Sparkle.framework "$app/Contents/Frameworks/Sparkle.framework"
xcrun actool "$assets/Turnscope.icon" --compile "$app/Contents/Resources" --app-icon Turnscope \
  --platform macosx --target-device mac --minimum-deployment-target "$minimum" \
  --output-partial-info-plist "$built/icon.plist" --output-format human-readable-text > /dev/null

if [ "${1:-}" = --release ]; then
  echo "Built Turnscope $version, unsigned, in $app"
else
  # Inside out, as scripts/sign-release.sh signs a release.
  codesign --force --sign - "$app/Contents/Frameworks/Sparkle.framework"
  codesign --force --sign - --identifier com.joeychilson.turnscope.cli "$app/Contents/Helpers/turnscope"
  codesign --force --sign - "$app"
  echo "Built $app. Open it on scratch data with:"
  echo "  open $app --args --data \"\$(mktemp -d)\""
fi
