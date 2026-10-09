#!/bin/bash
# Build the command line, `turnscope`, for the Xcode project's "Build
# turnscope" phase: once per architecture Xcode is building, in release for
# a Release build and debug otherwise, joined into one file that the next
# phase copies into Contents/Helpers and signs.
#
# Xcode sets ARCHS, CONFIGURATION and BUILT_PRODUCTS_DIR. Run by hand, it
# builds for this Mac in debug into target/helper/.
set -euo pipefail
cd "$(dirname "$0")/.."

# Xcode runs build phases without the login shell's PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

archs=${ARCHS:-$(uname -m)}
out=${BUILT_PRODUCTS_DIR:-target/helper}
profile=debug
flag=()
if [ "${CONFIGURATION:-Debug}" = Release ]; then
  profile=release
  flag=(--release)
fi

built=()
for arch in $archs; do
  case "$arch" in
    arm64) target=aarch64-apple-darwin ;;
    x86_64) target=x86_64-apple-darwin ;;
    *) echo "build-cli.sh: no Rust target for $arch" >&2; exit 1 ;;
  esac
  cargo build --locked -p turnscope --target "$target" ${flag[@]+"${flag[@]}"}
  built+=("${CARGO_TARGET_DIR:-target}/$target/$profile/turnscope")
done

mkdir -p "$out"
lipo -create -output "$out/turnscope" "${built[@]}"
