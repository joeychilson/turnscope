#!/usr/bin/env bash
# Build Turnscope for release into target/dist: the app for Apple silicon and
# Intel, signed, notarized and stapled; its disk image, the same; and the
# Sparkle appcast that offers it to every copy already installed.
#
#   scripts/release.sh
#
# What it needs comes from the environment, so it runs the same on this Mac
# and in the release workflow:
#
#   SIGN_IDENTITY       the Developer ID Application identity to sign with,
#                       by name or hash; without it everything is signed ad
#                       hoc, to try the build here, and nothing is notarized
#   NOTARY_KEY          an App Store Connect API key's .p8 file, with
#   NOTARY_KEY_ID       its key ID and
#   NOTARY_ISSUER       its issuer ID, to notarize; needed with SIGN_IDENTITY
#   SPARKLE_PUBLIC_KEY  the public half of the key updates are signed with,
#                       which the app carries to check them
#   SPARKLE_PRIVATE_KEY its private half, as `generate_keys -x` exports it, to
#                       sign the disk image and write the appcast; without
#                       it there is no appcast; needed with SIGN_IDENTITY
#
# The app is built unsigned and then signed inside out, as Sparkle asks of
# an app that isn't sandboxed: Sparkle's helpers, its framework, the command
# line, then the app, each with the hardened runtime and a secure timestamp.
# Sparkle's tools, which sign the update, are those of the version the Xcode
# project resolves, downloaded once into target/sparkle-<version>/.
set -euo pipefail
cd "$(dirname "$0")/.."

# The private key is kept out of the environment, so nothing the build runs,
# such as a dependency's build script, is handed it.
sparkle_key=${SPARKLE_PRIVATE_KEY:-}
unset SPARKLE_PRIVATE_KEY

version=$(cargo pkgid -p turnscope | sed 's/.*@//')
sparkle=$(jq -r '.pins[] | select(.identity == "sparkle") | .state.version' \
  macos/Turnscope.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved)
identity=${SIGN_IDENTITY:--}
dist=target/dist
app="$dist/Turnscope.app"
image="$dist/Turnscope-$version.dmg"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release.sh: Cargo.toml's version, $version, isn't X.Y.Z" >&2
  exit 2
fi
if ! [[ $sparkle =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "release.sh: the Xcode project resolves no Sparkle version ($sparkle)" >&2
  exit 2
fi
if [ "$identity" != - ]; then
  for name in NOTARY_KEY NOTARY_KEY_ID NOTARY_ISSUER SPARKLE_PUBLIC_KEY; do
    if [ -z "${!name:-}" ]; then
      echo "release.sh: a signed release needs $name (scripts/release.sh says what each is)" >&2
      exit 2
    fi
  done
  if [ -z "$sparkle_key" ]; then
    echo "release.sh: a signed release needs SPARKLE_PRIVATE_KEY (scripts/release.sh says what each is)" >&2
    exit 2
  fi
  if [ ! -f "$NOTARY_KEY" ]; then
    echo "release.sh: NOTARY_KEY names no file: $NOTARY_KEY" >&2
    exit 2
  fi
fi
if [ -n "$sparkle_key" ] && [ -z "${SPARKLE_PUBLIC_KEY:-}" ]; then
  echo "release.sh: SPARKLE_PRIVATE_KEY needs SPARKLE_PUBLIC_KEY, or no copy could check the update" >&2
  exit 2
fi

# The app, unsigned, for both architectures; the build phase builds the
# command line for both and joins them.
xcodebuild build \
  -project macos/Turnscope.xcodeproj -scheme Turnscope -configuration Release \
  -destination "generic/platform=macOS" -derivedDataPath target/xcode-release \
  -clonedSourcePackagesDirPath target/swiftpm \
  ARCHS="arm64 x86_64" ONLY_ACTIVE_ARCH=NO \
  CODE_SIGNING_ALLOWED=NO \
  MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$version" \
  SPARKLE_PUBLIC_KEY="${SPARKLE_PUBLIC_KEY:-}" \
  -quiet
rm -rf "$dist"
mkdir -p "$dist"
ditto target/xcode-release/Build/Products/Release/Turnscope.app "$app"

for executable in MacOS/Turnscope Helpers/turnscope; do
  arches=$(lipo -archs "$app/Contents/$executable")
  for want in arm64 x86_64; do
    case " $arches " in
      *" $want "*) ;;
      *) echo "release.sh: $executable was built without $want ($arches)" >&2; exit 1 ;;
    esac
  done
done

# A Developer ID's signature carries a secure timestamp, which keeps it valid
# after the certificate expires. An ad hoc one can't, and has no team, so the
# hardened runtime, which loads only frameworks of the app's own team, would
# refuse Sparkle: signed ad hoc, as Xcode does, it runs without it.
sign() {
  if [ "$identity" = - ]; then
    codesign --force --sign - "$@"
  else
    codesign --force --options runtime --timestamp --sign "$identity" "$@"
  fi
}
framework="$app/Contents/Frameworks/Sparkle.framework/Versions/B"
sign "$framework/XPCServices/Installer.xpc"
sign --preserve-metadata=entitlements "$framework/XPCServices/Downloader.xpc"
sign "$framework/Autoupdate"
sign "$framework/Updater.app"
sign "$app/Contents/Frameworks/Sparkle.framework"
sign --identifier com.joeychilson.turnscope.cli "$app/Contents/Helpers/turnscope"
sign "$app"
codesign --verify --deep --strict "$app"

# Submit `$1` to Apple's notary service and wait for its verdict; a refusal
# prints Apple's log of what it found.
notarize() {
  local answer id status
  answer=$(xcrun notarytool submit "$1" \
    --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" \
    --wait --timeout 45m --output-format json 2>&1) || true
  id=$(grep '^{' <<< "$answer" | tail -n 1 | plutil -extract id raw - 2>/dev/null) || id=""
  status=$(grep '^{' <<< "$answer" | tail -n 1 | plutil -extract status raw - 2>/dev/null) || status=""
  if [ "$status" != Accepted ]; then
    echo "release.sh: notarizing $(basename "$1") answered ${status:-nothing}: $answer" >&2
    [ -n "$id" ] && xcrun notarytool log "$id" \
      --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" >&2
    exit 1
  fi
  echo "Notarized $(basename "$1") ($id)"
}

if [ "$identity" != - ]; then
  ditto -c -k --keepParent "$app" "$work/Turnscope.zip"
  notarize "$work/Turnscope.zip"
  xcrun stapler staple -q "$app"
  spctl --assess --type execute "$app"
fi

# The disk image: the app beside a link to Applications.
mkdir "$work/image"
ditto "$app" "$work/image/Turnscope.app"
ln -s /Applications "$work/image/Applications"
hdiutil create -quiet -volname Turnscope -srcfolder "$work/image" -format UDZO "$image"
if [ "$identity" != - ]; then
  codesign --force --timestamp --sign "$identity" "$image"
  notarize "$image"
  xcrun stapler staple -q "$image"
  spctl --assess --type open --context context:primary-signature "$image"
fi

# The appcast: this release, signed with Sparkle's key, at the address every
# copy's SUFeedURL names, the latest release's appcast.xml.
if [ -n "$sparkle_key" ]; then
  tools="target/sparkle-$sparkle"
  if [ ! -x "$tools/bin/sign_update" ]; then
    # Unpacked beside it first, so a download cut short is never taken for
    # the tools.
    rm -rf "$tools" "$tools.partial"
    mkdir -p "$tools.partial"
    /usr/bin/curl -q --silent --show-error --fail --location --proto =https \
      "https://github.com/sparkle-project/Sparkle/releases/download/$sparkle/Sparkle-$sparkle.tar.xz" |
      tar -xJ -C "$tools.partial"
    mv "$tools.partial" "$tools"
  fi
  enclosure=$("$tools/bin/sign_update" --ed-key-file - "$image" <<< "$sparkle_key")
  signature=$(sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p' <<< "$enclosure")

  # The signature must pass the check every copy makes with the public key
  # it carries: a private key that isn't its pair would publish an update
  # every copy refuses.
  cat > "$work/check.swift" <<'SWIFT'
import CryptoKit
import Foundation
let given = CommandLine.arguments
guard given.count == 4,
      let key = Data(base64Encoded: given[1]),
      let signature = Data(base64Encoded: given[2]),
      let file = FileManager.default.contents(atPath: given[3]),
      let publicKey = try? Curve25519.Signing.PublicKey(rawRepresentation: key),
      publicKey.isValidSignature(signature, for: file)
else { exit(1) }
SWIFT
  if ! xcrun swift "$work/check.swift" "$SPARKLE_PUBLIC_KEY" "$signature" "$image"; then
    echo "release.sh: the update's signature fails with SPARKLE_PUBLIC_KEY: SPARKLE_PRIVATE_KEY isn't its pair" >&2
    exit 1
  fi

  minimum=$(plutil -extract LSMinimumSystemVersion raw "$app/Contents/Info.plist")
  releases=https://github.com/joeychilson/turnscope/releases
  cat > "$dist/appcast.xml" <<XML
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Turnscope</title>
    <item>
      <title>Turnscope $version</title>
      <pubDate>$(LC_ALL=C date -u "+%a, %d %b %Y %H:%M:%S +0000")</pubDate>
      <sparkle:version>$version</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$minimum</sparkle:minimumSystemVersion>
      <sparkle:releaseNotesLink>$releases/tag/v$version</sparkle:releaseNotesLink>
      <enclosure url="$releases/download/v$version/$(basename "$image")" $enclosure type="application/octet-stream"/>
    </item>
  </channel>
</rss>
XML
  xmllint --noout "$dist/appcast.xml"
fi

echo "Built Turnscope $version in $dist:"
ls -1 "$dist"
