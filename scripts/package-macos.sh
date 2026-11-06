#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

APP="dist/IPv6 DDNS.app"
BUNDLE="$APP/Contents"
cargo build --locked --release

rm -rf "$APP"
mkdir -p "$BUNDLE/MacOS" "$BUNDLE/Resources"
cp target/release/ipv6-ddns "$BUNDLE/MacOS/IPv6-DDNS"
chmod 755 "$BUNDLE/MacOS/IPv6-DDNS"
cp assets/macos/Info.plist "$BUNDLE/Info.plist"
printf 'APPL????' > "$BUNDLE/PkgInfo"

if [ -f assets/macos/AppIcon.icns ]; then
	cp assets/macos/AppIcon.icns "$BUNDLE/Resources/AppIcon.icns"
fi

/usr/bin/plutil -lint "$BUNDLE/Info.plist" >/dev/null
if command -v codesign >/dev/null 2>&1; then
	codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true
fi

echo "$APP"
