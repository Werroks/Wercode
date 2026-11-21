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
# A desktop has every library Wercode uses; a minimal system (a server, WSL)
# may lack the keyboard ones for X11.
if command -v ldd > /dev/null; then
    missing="$(ldd "$bin/wercode" 2>/dev/null | awk '/not found/ { print $1 }')"
    if [ -n "$missing" ]; then
        echo "Wercode needs libraries this system does not have:"
        echo "$missing" | sed 's/^/  /'
        echo "On Debian or Ubuntu: sudo apt install libxkbcommon-x11-0 libxcb-xkb1"
    fi
fi
case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "Add $bin to PATH to start it as wercode from a terminal." ;;
esac
