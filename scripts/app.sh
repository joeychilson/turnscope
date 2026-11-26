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
set -euo pipefail
cd "$(dirname "$0")/.."

# A private key in the environment isn't handed to what the build runs.
unset SPARKLE_PRIVATE_KEY

assets=apps/macos/assets
header=crates/turnscope-app/include/turnscope_app.h
target_dir=${CARGO_TARGET_DIR:-target}

# `cargo pkgid` ends `#0.3.0` for a path package and `@0.3.0` for others.
version=$(cargo pkgid -p turnscope | sed 's/.*[#@]//')
if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "app.sh: Cargo.toml's version, $version, isn't X.Y.Z" >&2
  exit 2
fi
minimum=$(plutil -extract LSMinimumSystemVersion raw "$assets/Info.plist")
export MACOSX_DEPLOYMENT_TARGET=$minimum

swift_files=()
while IFS= read -r file; do
  swift_files+=("$file")
done < <(find apps/macos/Sources -name '*.swift' | sort)

scripts/sparkle.sh

compile_swift() {
  local swift_arch=$1 rust_lib=$2 out=$3 mode=$4
  local opt_flags=()
  if [ "$mode" = release ]; then
    opt_flags=(-O -whole-module-optimization -Xlinker -dead_strip)
  else
    opt_flags=(-Onone -g)
  fi
  xcrun swiftc \
    -swift-version 6 \
    -warnings-as-errors \
    -parse-as-library \
    "${opt_flags[@]}" \
    -target "${swift_arch}-apple-macosx${minimum}" \
    -import-objc-header "$header" \
    -F target/sparkle \
    -framework Sparkle \
    -framework CoreServices \
    -Xlinker -rpath -Xlinker @executable_path/../Frameworks \
    "${swift_files[@]}" \
    "$rust_lib" \
    -o "$out"
}

built=$(mktemp -d)
trap 'rm -rf "$built"' EXIT
case "${1:-}" in
  "")
    app=target/debug/Turnscope.app
    cargo build --locked -p turnscope -p turnscope-app
    cp "$target_dir/debug/turnscope" "$built/turnscope"
    compile_swift "$(uname -m)" "$target_dir/debug/libturnscope_app.a" "$built/turnscope-app" debug
    ;;
  --release)
    app=target/dist/Turnscope.app
    # Each binary for both architectures, joined into one universal file.
    cargo build --locked --release -p turnscope -p turnscope-app \
      --target aarch64-apple-darwin --target x86_64-apple-darwin
    compile_swift arm64 "$target_dir/aarch64-apple-darwin/release/libturnscope_app.a" "$built/turnscope-app-arm64" release
    compile_swift x86_64 "$target_dir/x86_64-apple-darwin/release/libturnscope_app.a" "$built/turnscope-app-x86_64" release
    lipo -create -output "$built/turnscope-app" "$built/turnscope-app-arm64" "$built/turnscope-app-x86_64"
    strip -x "$built/turnscope-app"
    lipo -create -output "$built/turnscope" \
      "$target_dir"/{aarch64,x86_64}-apple-darwin/release/turnscope
    ;;
  *)
    echo "usage: scripts/app.sh [--release]" >&2
    exit 2
    ;;
esac

rm -rf "$app"
mkdir -p \
  "$app/Contents/MacOS" \
  "$app/Contents/Helpers" \
  "$app/Contents/Frameworks" \
  "$app/Contents/Resources/logos/agents" \
  "$app/Contents/Resources/logos/providers"
plist="$app/Contents/Info.plist"
cp "$assets/Info.plist" "$plist"
plutil -replace CFBundleShortVersionString -string "$version" "$plist"
plutil -replace CFBundleVersion -string "$version" "$plist"
plutil -replace SUPublicEDKey -string "${SPARKLE_PUBLIC_KEY:-}" "$plist"
cp "$built/turnscope-app" "$app/Contents/MacOS/Turnscope"
# Connected agents run the command line by this path, so it never moves.
cp "$built/turnscope" "$app/Contents/Helpers/turnscope"
cp assets/logos/agents/*.svg "$app/Contents/Resources/logos/agents/"
cp assets/logos/providers/*.svg "$app/Contents/Resources/logos/providers/"
ditto target/sparkle/Sparkle.framework "$app/Contents/Frameworks/Sparkle.framework"
# Absolute paths keep Xcode's shared ibtoold daemon from resolving against another worktree.
xcrun actool "$PWD/$assets/Turnscope.icon" --compile "$PWD/$app/Contents/Resources" --app-icon Turnscope \
  --platform macosx --target-device mac --minimum-deployment-target "$minimum" \
  --output-partial-info-plist "$built/icon.plist" --output-format human-readable-text > /dev/null

if [ "${1:-}" = --release ]; then
  echo "Built Turnscope $version, unsigned, in $app"
else
  # Inside out, as scripts/sign-release.sh signs a release.
  codesign --force --sign - "$app/Contents/Frameworks/Sparkle.framework"
  codesign --force --sign - --identifier com.joeychilson.turnscope.cli "$app/Contents/Helpers/turnscope"
  codesign --force --sign - "$app"
  echo "Built $app. To run it on scratch data, see docs/development.md."
fi
