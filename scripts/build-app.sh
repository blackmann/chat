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
<key>CFBundleIdentifier</key><string>com.chat.desktop</string>
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
codesign --force --deep --sign - "$app"
printf 'Built %s\n' "$app"
