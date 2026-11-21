#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mode="${CHAT_BUILD_MODE:-release}"
case "$mode" in
  debug) cargo build --locked ;;
  release) cargo build --release --locked ;;
  *) printf "Unknown build mode: %s\n" "$mode" >&2; exit 1 ;;
esac
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
app="build/Chat.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "target/$mode/chat" "$app/Contents/MacOS/chat"
icon_key=""
if [ -f assets/AppIcon.icns ]; then
  cp assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"
  icon_key="<key>CFBundleIconFile</key><string>AppIcon</string>"
fi
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>chat</string>
<key>CFBundleIdentifier</key><string>uk.co.degreat.Chat</string>
<key>CFBundleName</key><string>Chat</string>
<key>CFBundleDisplayName</key><string>Chat</string>
$icon_key
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
# macOS remembers privacy grants (Documents, Screen Recording, ...) by the
# app's signature. An ad-hoc signature is just a hash of this build, so every
# update would look like a new app and ask again. A real identity keeps the
# grants across builds: CHAT_SIGN_IDENTITY if set, else the first Developer ID
# or Apple Development certificate in the keychain.
identity="${CHAT_SIGN_IDENTITY:-}"
if [ -z "$identity" ]; then
  identities=$(security find-identity -v -p codesigning 2>/dev/null || true)
  for kind in "Developer ID Application" "Apple Development"; do
    identity=$(printf '%s\n' "$identities" | sed -n "s/.*\"\($kind: .*\)\"/\1/p" | head -n 1)
    [ -n "$identity" ] && break
  done
fi
if [ -z "$identity" ]; then
  identity="-"
  printf 'No signing identity found: signing ad hoc, so macOS will ask for permissions again after each update.\n' >&2
fi
codesign --force --deep --sign "$identity" "$app"
printf 'Built %s (signed by %s)\n' "$app" "$identity"
