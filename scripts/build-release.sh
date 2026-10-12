#!/bin/bash
# Build Turnscope's app for release into target/dist/Turnscope.app, for Apple
# silicon and Intel, unsigned, with the Xcode project's packages resolved
# into target/swiftpm/. scripts/sign-release.sh then signs it, and makes its
# disk image and appcast:
#
#   scripts/build-release.sh && scripts/sign-release.sh
#
#   SPARKLE_PUBLIC_KEY  the public half of the key updates are signed with,
#                       which the app carries to check them
set -euo pipefail
cd "$(dirname "$0")/.."

# A private key in the environment isn't handed to what the build runs.
unset SPARKLE_PRIVATE_KEY

# `cargo pkgid` ends `#0.1.0` for a path package and `@0.1.0` for others.
version=$(cargo pkgid -p turnscope | sed 's/.*[#@]//')
app=target/dist/Turnscope.app

if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "build-release.sh: Cargo.toml's version, $version, isn't X.Y.Z" >&2
  exit 2
fi

# Its build phase builds the command line for both architectures and joins
# them into one file.
xcodebuild build \
  -project macos/Turnscope.xcodeproj -scheme Turnscope -configuration Release \
  -destination "generic/platform=macOS" -derivedDataPath target/xcode-release \
  -clonedSourcePackagesDirPath target/swiftpm \
  ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO \
  CODE_SIGNING_ALLOWED=NO \
  MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$version" \
  SPARKLE_PUBLIC_KEY="${SPARKLE_PUBLIC_KEY:-}" \
  -quiet
rm -rf target/dist
mkdir -p target/dist
ditto target/xcode-release/Build/Products/Release/Turnscope.app "$app"

for executable in MacOS/Turnscope Helpers/turnscope; do
  arches=$(lipo -archs "$app/Contents/$executable")
  for want in arm64 x86_64; do
    case " $arches " in
      *" $want "*) ;;
      *) echo "build-release.sh: $executable was built without $want ($arches)" >&2; exit 1 ;;
    esac
  done
done

echo "Built Turnscope $version, unsigned, in $app"
