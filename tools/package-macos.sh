#!/bin/bash
# Run on the architecture being packaged. GPUI embeds its Metal library and artwork.
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
  echo 'Package TokenBubble with a native macOS Rust toolchain.' >&2
  exit 1
fi
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
case "$(uname -m)" in
  arm64) native_target=aarch64-apple-darwin; arch=aarch64 ;;
  x86_64) native_target=x86_64-apple-darwin; arch=x86_64 ;;
  *) echo 'Unsupported macOS architecture.' >&2; exit 1 ;;
esac
target="${1:-$native_target}"
if [[ "$target" != "$native_target" ]]; then
  echo "Expected native target $native_target, got $target." >&2
  exit 1
fi
export MACOSX_DEPLOYMENT_TARGET=13.0
metadata="$(cargo metadata --locked --no-deps --format-version 1)"
version="$(printf '%s' "$metadata" | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "tokenbubble-gpui"))')"
short_version="${version%%-*}"
# Apple's build-version grammar supports bN, rather than SemVer's -beta.N.
bundle_version="$short_version"
if [[ "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)-beta\.([0-9]+)$ ]]; then
  bundle_version="${BASH_REMATCH[1]}b${BASH_REMATCH[2]}"
fi
cargo build --release --locked --target "$target" -p tokenbubble-gpui --bin tokenbubble

output="$root/target/release-packages"
mkdir -p "$output"
staging="$(mktemp -d "${TMPDIR:-/tmp}/tokenbubble-package.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
app="$staging/TokenBubble.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$target/release/tokenbubble" "$app/Contents/MacOS/tokenbubble"
chmod 755 "$app/Contents/MacOS/tokenbubble"
cp THIRD_PARTY_NOTICES.md "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"
cp THIRD_PARTY_NOTICES.md "$staging/THIRD_PARTY_NOTICES.md"
swift tools/macos-icon.swift "$staging/TokenBubble.iconset"
iconutil -c icns "$staging/TokenBubble.iconset" -o "$app/Contents/Resources/TokenBubble.icns"
rm -rf "$staging/TokenBubble.iconset"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>TokenBubble</string>
  <key>CFBundleName</key><string>TokenBubble</string>
  <key>CFBundleExecutable</key><string>tokenbubble</string>
  <key>CFBundleIdentifier</key><string>dev.cloudboy.tokenbubble</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>TokenBubble</string>
  <key>CFBundleShortVersionString</key><string>$short_version</string>
  <key>CFBundleVersion</key><string>$bundle_version</string>
  <key>CFBundleGetInfoString</key><string>TokenBubble $version</string>
  <key>TokenBubbleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"
cat > "$staging/INSTALL.txt" <<INSTALL
TokenBubble $version - macOS $arch (macOS 13 Ventura or newer)

1. Extract the ZIP and move TokenBubble.app to /Applications or ~/Applications.
2. Open TokenBubble.app. The bubbles emoji appears in the menu bar, not the Dock.
   Left-click it for usage; right-click it for Refresh, Settings, About, and Quit.
3. Select Codex or Claude and sign in with its native macOS CLI. TokenBubble reads
   the CLI's credentials; it does not manage logins or renew credentials.

This app is ad-hoc signed, NOT Developer ID signed or notarized. Gatekeeper may
block it. Download only from https://github.com/cloudboy-jh/tokenbubble/releases
and verify the ZIP against SHA256SUMS.txt before opening it:
  shasum -a 256 tokenbubble-v$version-macos-$arch.zip

If you trust this download, attempt to open it, then use System Settings >
Privacy & Security > Open Anyway. Do not disable Gatekeeper globally. Managed
Macs may prohibit this override. There is no paid signing identity in this build.

Updates are manual: quit from the menu-bar menu, replace the installed app, then
reopen it. Settings stay in ~/Library/Application Support/TokenBubble/settings.toml.
CLI credentials are not part of this package. Start at sign-in is not supported
on macOS in this release.
INSTALL
# All bundle contents must be final before signing. No entitlements or notarization.
codesign --force --sign - --timestamp=none "$app"
codesign --verify --deep --strict --verbose=2 "$app"
name="tokenbubble-v$version-macos-$arch"
archive="$output/$name.zip"
# ditto preserves the signed bundle, permissions and macOS metadata.
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
# Add the human-readable files alongside the .app, not inside its signed seal.
(cd "$staging" && /usr/bin/zip -q "$archive" INSTALL.txt THIRD_PARTY_NOTICES.md)
(cd "$output" && shasum -a 256 "$name.zip" > SHA256SUMS.txt)
printf 'Package: %s\n' "$archive"
