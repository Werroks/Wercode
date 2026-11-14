#!/bin/sh
# Makes Wercode.app from the Apple silicon and Intel release builds, and packs
# it twice: a disk image for people, an archive for the app's own updates.
#   sh packaging/macos/bundle.sh 0.1.0
set -eu
version="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/dist"
app="$out/Wercode.app"
mkdir -p "$out"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

lipo -create -output "$app/Contents/MacOS/wercode" \
    "$root/target/aarch64-apple-darwin/release/wercode" \
    "$root/target/x86_64-apple-darwin/release/wercode"
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"

# Every icon size macOS asks for, from one large picture.
iconset="$out/wercode.iconset"
rm -rf "$iconset"
mkdir -p "$iconset"
picture="$root/assets/brand/wercode-macos.png"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$picture" --out "$iconset/icon_${size}x${size}.png" > /dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$picture" --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/wercode.icns"
rm -rf "$iconset"

# No Developer ID yet. An ad-hoc signature is still needed: Apple silicon
# refuses to run code with none at all.
codesign --force --deep --sign - "$app"

# COPYFILE_DISABLE keeps macOS from adding ._ files to the archive.
COPYFILE_DISABLE=1 tar -czf "$out/Wercode-$version-macos.tar.gz" -C "$out" Wercode.app

stage="$out/dmg"
rm -rf "$stage"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname "Wercode $version" -srcfolder "$stage" -ov -format UDZO "$out/Wercode-$version-macos.dmg"
rm -rf "$stage"
