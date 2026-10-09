#!/bin/bash
# Build the app for this Mac, for development: target/xcode/Build/Products/
# Debug/Turnscope.app, signed ad hoc, warnings as errors. Open it on a
# scratch data directory with
#
#   open target/xcode/Build/Products/Debug/Turnscope.app --args --data <dir> --open
#
# Or open macos/Turnscope.xcodeproj in Xcode and run the Turnscope scheme.
# Swift packages are resolved into target/swiftpm/, which build-release.sh shares.
set -euo pipefail
cd "$(dirname "$0")/.."
# The version is the crate's, as build-release.sh gives it.
version=$(cargo pkgid -p turnscope | sed 's/.*[#@]//')
xcodebuild build -project macos/Turnscope.xcodeproj -scheme Turnscope -configuration Debug \
  MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$version" \
  -destination "platform=macOS,arch=$(uname -m)" \
  -derivedDataPath target/xcode -clonedSourcePackagesDirPath target/swiftpm -quiet
echo "Built target/xcode/Build/Products/Debug/Turnscope.app"
