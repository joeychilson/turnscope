#!/usr/bin/env bash
# Build the app for this Mac, for development: target/xcode/Build/Products/
# Debug/Turnscope.app, signed ad hoc, warnings as errors. Open it on a
# scratch ledger with
#
#   open target/xcode/Build/Products/Debug/Turnscope.app --args --data <dir> --open
#
# Or open macos/Turnscope.xcodeproj in Xcode and run the Turnscope scheme.
# Swift packages are resolved into target/swiftpm/, which release.sh shares.
set -euo pipefail
cd "$(dirname "$0")/.."
xcodebuild build -project macos/Turnscope.xcodeproj -scheme Turnscope -configuration Debug \
  -destination "platform=macOS,arch=$(uname -m)" \
  -derivedDataPath target/xcode -clonedSourcePackagesDirPath target/swiftpm -quiet
echo "Built target/xcode/Build/Products/Debug/Turnscope.app"
