#!/bin/bash
# Builds the finished KILN.app and an installer disk image in dist/.
# Run from the app folder: bash package/make_app.sh
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST=dist
APP="$DIST/KILN.app"
FFMPEG=${FFMPEG:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}
FFPROBE=${FFPROBE:-$(command -v ffprobe || echo /opt/homebrew/bin/ffprobe)}

echo "== icon"
cargo run --release --example make_icon
ICONSET=$(mktemp -d)/KILN.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon-1024.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d assets/icon-1024.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done

echo "== build"
cargo build --release

echo "== bundle"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/Licenses" "$APP/Contents/Frameworks"
cp target/release/kiln "$APP/Contents/MacOS/KILN"
cp Info.plist "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/KILN.icns"
cp assets/fonts/OFL-*.txt "$APP/Contents/Resources/Licenses/"
FFROOT=$(cd "$(dirname "$(realpath "$FFMPEG")")/.." && pwd)
for f in LICENSE.md COPYING.GPLv2 COPYING.GPLv3 COPYING.LGPLv2.1 COPYING.LGPLv3; do
  [ -f "$FFROOT/$f" ] && cp "$FFROOT/$f" "$APP/Contents/Resources/Licenses/FFmpeg-$f"
done
python3 -I package/bundle_ffmpeg.py "$APP" "$FFMPEG" "$FFPROBE"

echo "== sign (ad hoc)"
find "$APP/Contents/Frameworks" -type f -name '*.dylib' -exec codesign --force --sign - {} \;
codesign --force --sign - "$APP/Contents/MacOS/ffmpeg"
codesign --force --sign - "$APP/Contents/MacOS/ffprobe"
codesign --force --sign - --identifier app.kiln.analog-video-emulator "$APP"
codesign --verify --deep --strict "$APP"

echo "== check the bundled tools run"
"$APP/Contents/MacOS/ffmpeg" -hide_banner -version | head -1
"$APP/Contents/MacOS/ffprobe" -hide_banner -version | head -1

echo "== disk image"
STAGE=$(mktemp -d)/KILN
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cp package/ReadMe.txt "$STAGE/Read Me.txt"
DMG="$DIST/KILN-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "KILN $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
echo "made $APP and $DMG ($(du -sh "$DMG" | cut -f1))"
