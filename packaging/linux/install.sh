#!/bin/sh
# Installs Wercode for the current user: the program into ~/.local/bin, and
# a menu entry with its icon. Run it from the unpacked folder:
#   ./install.sh
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/512x512/apps"
install -m 755 "$here/wercode" "$bin/wercode"
install -m 644 "$here/wercode.png" "$data/icons/hicolor/512x512/apps/wercode.png"
sed "s|^Exec=wercode|Exec=$bin/wercode|" "$here/wercode.desktop" > "$data/applications/wercode.desktop"
if command -v update-desktop-database > /dev/null; then
    update-desktop-database "$data/applications" || true
fi
echo "Wercode is installed: $bin/wercode"
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "Add $bin to PATH to start it as wercode from a terminal." ;;
esac
