#!/bin/sh
# Packs the Linux release build with its menu entry, icon and install script.
#   sh packaging/linux/package.sh 0.1.0
set -eu
version="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/dist"
stage="$out/wercode"
rm -rf "$stage"
mkdir -p "$stage"
cp "$root/target/release/wercode" "$stage/wercode"
cp "$root/packaging/linux/wercode.desktop" "$stage/"
cp "$root/packaging/linux/install.sh" "$stage/"
cp "$root/assets/brand/wercode-512.png" "$stage/wercode.png"
chmod 755 "$stage/wercode" "$stage/install.sh"
tar -czf "$out/Wercode-$version-linux-x64.tar.gz" -C "$out" wercode
rm -rf "$stage"
