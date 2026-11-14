## Install

- **Windows 10 and 11:** `Wercode-Setup-<version>-x64.exe`. It installs for your user only, no administrator rights needed. The installer is not signed yet, so Windows SmartScreen may say "Windows protected your PC": click **More info**, then **Run anyway**.
- **macOS 11 and later (Apple silicon and Intel):** `Wercode-<version>-macos.dmg`, then drag Wercode to Applications. The app is not notarized yet: the first time, open it, then go to **System Settings → Privacy & Security** and click **Open Anyway**.
- **Linux (x86-64):** `Wercode-<version>-linux-x64.tar.gz`. Unpack it and run `./install.sh` inside: it puts `wercode` into `~/.local/bin` and adds a menu entry.

Wercode updates itself: new versions download in the background and are installed when it restarts. This can be turned off in Settings.
