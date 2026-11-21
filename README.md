<p align="center">
  <img src="assets/brand/wercode-512.png" alt="Wercode" width="112">
</p>

<h1 align="center">Wercode</h1>

<p align="center">
  <b>The terminal-first code editor.</b><br>
  Your terminal in the middle, with files, git and code checks around it.<br>
  Native, written in Rust. Ready in about half a second, on about 20 MB of memory.<br>
  No browser engine inside.
</p>

<p align="center">
  <a href="https://github.com/Werroks/Wercode/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Werroks/Wercode?label=download&color=2ea44f"></a>
  <img alt="Windows, macOS, Linux" src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-informational">
  <img alt="Written in Rust" src="https://img.shields.io/badge/written%20in-Rust-orange">
  <a href="#license"><img alt="License: MIT or Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue"></a>
</p>

<p align="center">
  <img src="docs/demo.gif" alt="Wercode: cargo run in the terminal, git merge stops on a conflict, Accept Both resolves it, cargo run shows both changes and the merge appears in the commit graph" width="900"><br>
  <sub>A merge conflict, start to finish: run, merge, <b>Accept Both</b>, run again, commit.</sub>
</p>

---

More and more code gets written by an agent running in a terminal: Claude Code,
Codex, Aider, Gemini CLI. The editors we use were built for typing every line
yourself, so the terminal sits in a strip at the bottom and the agent's work
shows up in a window that was never meant for it.

**Wercode turns that around.** The terminal takes the middle of the window, with
tabs and splits that come back after a restart. Next to it: the files of your
project with their git status, a git panel with a commit graph, an editor for
the edits you still make by hand, and the errors and warnings of your code.
Everything opens at once, because there is no Chromium underneath, just Rust
drawing on your GPU.

## Light and fast

Wercode is one native program. VS Code and Cursor each carry a copy of the
Chromium browser and Node.js, and start eight to fourteen processes to show you
a folder. We opened the same project (this repository) in all three on one
Windows 11 computer:

|  | **Wercode 0.6** | VS Code 1.104 | Cursor 3.31 |
| --- | --- | --- | --- |
| Memory, as Task Manager shows it | **about 20 MB** | about 530 MB | about 1.5 GB¹ |
| Window fully drawn after launch | **0.6 s** | 2.5 s | 3.1 s¹ |
| Processes | **1** | 8 | 13–14 |
| Windows installer | **11.3 MB** | 286 MB | 208 MB |

That is **about 25 times less memory than VS Code and over 70 times less than
Cursor**, and a window you can work in four times sooner. Your RAM stays free
for what really needs it: the agent, the compiler, the browser with the docs.
It does not grow with new features either: on 15 November, four releases ago,
Wercode's working set was about 70 MB, and with everything added since (all
changes in one tab, merge conflicts, worktrees, two files side by side, quick
fixes and more) it still is.

<sub>Measured on 20 November 2026. VS Code and Cursor ran with a new profile and no
extensions; a real setup with extensions uses more. Times are averages of several
runs after a warm-up; "fully drawn" is when the window stopped changing. Memory is
the private working set of the program's own processes 20 seconds after launch,
added up the way Task Manager does; counting the shared system libraries too,
Wercode's working set is about 70 MB and VS Code's about 1 GB. The shell inside
Wercode's terminal is not counted (VS Code had no terminal open). Installer sizes
of VS Code and Cursor are for the versions current on 15 November 2026 (1.146 and 3.30).<br>
¹ Without an account Cursor stops at its sign-in screen, so its figures are for
that screen.</sub>

## Why not just VS Code or Cursor?

They are good editors. But:

- **They are web pages in a box.** VS Code and Cursor run on Electron: a copy of
  the Chromium browser and Node.js behind every window. Wercode is a native
  program built on [GPUI](https://www.gpui.rs/), the GPU-rendered UI framework
  behind the Zed editor, so it needs a fraction of their memory and time
  (see the numbers above).
- **The terminal is an afterthought.** In both it is a panel under the editor.
  In Wercode it is the main stage: tabs, splits, find in output, clickable paths
  and links, each terminal's folder and shell restored next time.
- **Cursor's AI needs an account and, past a free tier, a subscription,** and
  it sits in the middle of everything. Wercode has no AI built in, on purpose:
  run whichever agent you like in the terminal. No account, no sign-in. Wercode itself sends
  nothing anywhere except the update check, which you can turn off.
- **A good VS Code setup is a pile of extensions,** and each one adds to the
  start-up time. In Wercode the essentials are built in.

|  | **Wercode** | VS Code | Cursor |
| --- | --- | --- | --- |
| Built with | Rust, GPUI (native) | Electron | Electron (VS Code fork) |
| Terminal | in the centre, tabs and splits | bottom panel | bottom panel |
| AI | any CLI agent, your choice | Copilot | built in, paid plans |
| Account needed | no | no | for its AI features |
| Open source | yes, MIT / Apache-2.0 | core is MIT, the build is not | no |

**When to stay with VS Code:** you depend on its extensions, a debugger, remote
development or notebooks. Wercode does not do these, and is not trying to be a
smaller VS Code.

## What is inside

<p align="center">
  <img src="docs/screenshot.png" alt="Wercode: an editor above a terminal, with the git panel and the commit graph on the left" width="900">
</p>

**Agents in the terminal**
- Claude Code and Codex work right in the terminal, the way they do in Cursor: **Shift+Enter**
  (or Ctrl+Enter) starts a new line of your message instead of sending it, on Windows with any
  keyboard layout, on macOS and on Linux
- **Pasting a screenshot** (Ctrl+V on Windows, Cmd+V on macOS, Ctrl+Shift+V on Linux) saves it
  and hands the agent its path, which Claude Code and Codex attach as `[Image #1]`. Image files
  dragged onto the terminal work the same way

**Terminal**
- Your own shell: PowerShell, cmd, Git Bash, WSL, bash, zsh, fish (found automatically)
- Tabs you can rename and drag; up to four splits per tab
- Find in the output, Ctrl+click on links and on `file:line` from compiler output
- Tabs, splits and each terminal's folder come back after a restart
- Tasks: the scripts of `package.json`, Cargo and Go commands, `Makefile` targets and `justfile`
  recipes in one list (Ctrl+Shift+X); the chosen one runs in a new terminal tab

**Files and editor**
- File tree with git status colours, `.gitignore` respected, follows the disk live
- Go to any file by a few letters of its name, search and replace across the whole project
- Built-in editor with syntax colours for 20 languages, find and replace, undo
- **Two files side by side** in one tab, to the right or below, with a line to drag between
  them: the buttons on the file's tab bar, **Open to the Side** in the file tree, or drop a tab
  onto a file
- Pictures (PNG, JPEG, GIF, WebP, BMP, ICO, TIFF) open as pictures
- Notices when an agent changes an open file and reloads it
- Marks next to the line numbers show what changed since the last commit; the status bar
  says who last changed the line under the cursor, and when

**Git**
- **All changes in one tab** (Ctrl+Shift+A): every changed file one after another, to read what
  an agent did in one place, with **Stage**, **Discard** (and **Undo**) and **Unstage** under
  each piece
- **Merge conflicts with buttons:** **Accept Current**, **Accept Incoming** or **Accept Both**
  over each conflict; the last one stages the file
- Stage, unstage, commit, discard; push, pull and fetch
- Commit graph with branches and tags; click a commit to see what it changed
- Branch, commits to push and pull, and changes in the status bar; switch, create and delete
  branches, and check out the ones that are only on the remote
- **Worktrees:** a folder for each branch, made and switched from the status bar, so several
  agents can work on different branches at once

**Code checks**
- Errors and warnings underlined as you type, with the message on hover
- A Problems panel with everything the checkers found in the project
- Rest the pointer on a name for its type and documentation; F12 or Ctrl+click goes to its definition
- Completion as you type (or Ctrl+Space), and formatting when a file is saved if you turn it on
- Quick fixes (Ctrl+.), rename across the project (F2), go to a symbol (Ctrl+R), the path to
  the cursor above the code, and inferred types in grey at the end of the line
- Uses the language servers already on your computer, or downloads a missing one with one
  click, no npm or rustup needed (see below)

**Everything else**
- Several windows at once, each with its own project (Ctrl+Shift+N or the button in the title bar)
- Command palette (Ctrl+Shift+P); every shortcut can be changed in Settings: click it and
  press the new keys
- Five colour schemes, plus your own (Settings → Colour scheme → **New Scheme…**); interface
  in English, Russian, German, Spanish and Chinese
- Updates itself in the background

## Download

Get the latest version from the **[Releases page](https://github.com/Werroks/Wercode/releases/latest)**.

| System | File | Notes |
| --- | --- | --- |
| Windows 10 and 11 | `Wercode-Setup-<version>-x64.exe` | Installs for your user, no admin rights. Not signed yet: if SmartScreen complains, click **More info → Run anyway**. |
| macOS 11+ (Apple silicon and Intel) | `Wercode-<version>-macos.dmg` | Not notarized yet: open it once, then **System Settings → Privacy & Security → Open Anyway**. |
| Linux x86-64 | `Wercode-<version>-linux-x64.tar.gz` | Unpack and run `./install.sh`: it puts `wercode` in `~/.local/bin` and adds a menu entry. A minimal system (a server, WSL) may first need `sudo apt install libxkbcommon-x11-0 libxcb-xkb1`; `install.sh` names any library that is missing. |

Then open a folder from the welcome screen, or from a terminal:

```sh
wercode path/to/project
```

## Code checks (linters)

Wercode talks to **language servers**, the same programs VS Code and Zed use
for errors, warnings and lint rules, for what a name means and for where it
is defined. It does not bundle them, so the download stays small: it finds the
ones installed on your computer and starts them when you open a file of their
language. If one is missing, the status bar says so, and **Install** downloads
it into Wercode's own folder: no npm, rustup or administrator rights needed.
The servers written for Node.js come with a Node.js that Wercode downloads too.
For Go and Ruby, **Install** runs the command below in a terminal tab.

| Language | Server | If it is not on your computer |
| --- | --- | --- |
| Rust | rust-analyzer (with `cargo check` on save) | downloaded by Wercode |
| TypeScript, JavaScript | typescript-language-server | downloaded by Wercode |
| Python | basedpyright, pyright or pylsp; also `ruff` if installed | pyright downloaded by Wercode |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` |
| C, C++ | clangd | downloaded by Wercode |
| JSON, CSS, HTML | vscode-langservers-extracted | downloaded by Wercode |
| YAML | yaml-language-server | downloaded by Wercode |
| Shell | bash-language-server | downloaded by Wercode |
| TOML | taplo | downloaded by Wercode |
| Ruby | ruby-lsp | `gem install ruby-lsp` |

Checking can be turned off in Settings → **Check code**. **Format on save**, next
to it, lets the server lay the code out each time you save (for Rust that is
`rustfmt`: `rustup component add rustfmt`).

## Keyboard

Plain Ctrl keys belong to your shell, so Wercode's own shortcuts use Ctrl+Shift.
All of them can be changed in Settings → **Keyboard shortcuts**: click one and press
the new keys (or edit the `[keys]` table of `settings.toml`).

| Shortcut | Does |
| --- | --- |
| Ctrl+Shift+P | Command palette |
| Ctrl+Shift+K | Go to file |
| Ctrl+Shift+F | Find in files |
| Ctrl+Shift+G | Git panel |
| Ctrl+Shift+A | All changes in one tab |
| Ctrl+Shift+M | Problems |
| Ctrl+Shift+B | Show or hide the side panel |
| Shift+Enter or Ctrl+Enter | New line in the prompt of Claude Code, Codex or PowerShell |
| Ctrl+V (Cmd+V on macOS, Ctrl+Shift+V on Linux) | Paste text, or a screenshot as an image for the agent |
| Ctrl+Shift+N | New window |
| Ctrl+Shift+O | Open a folder |
| Ctrl+Shift+T / Ctrl+Shift+Q | New terminal tab / close the tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Ctrl+Shift+D / Ctrl+Shift+E | Split right / split down |
| Ctrl+Shift+W | Close the split |
| Ctrl+Shift+S | Find in terminal output |
| Ctrl+Shift+X / Ctrl+Shift+L | Run a task / run the last task again |
| Ctrl+Shift+J | Show or hide the terminal (terminal-below layout) |
| Ctrl+Shift+Enter | Commit |
| Ctrl+Shift+Up / Down / R | Push / pull / fetch |
| Ctrl+S | Save (in the editor) |
| F12 or Ctrl+click | Go to definition (in the editor) |
| Ctrl+Space | Completion (in the editor) |
| Ctrl+. | Quick fixes (in the editor) |
| F2 | Rename everywhere in the project (in the editor) |
| Ctrl+R | Go to a symbol in the file (in the editor) |
| Ctrl+Alt+arrows | Go to the terminal on that side of a split |

## Build from source

You need a recent stable [Rust](https://rustup.rs) and:

- **Windows:** Visual Studio Build Tools with the C++ workload
- **macOS:** Xcode command line tools (`xcode-select --install`)
- **Linux (Debian, Ubuntu):**
  `sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libx11-dev libx11-xcb-dev libfontconfig1-dev libfreetype6-dev libasound2-dev libssl-dev libvulkan-dev pkg-config`

```sh
git clone https://github.com/Werroks/Wercode
cd Wercode
cargo run --release -- path/to/project
```

The first build compiles the UI framework and takes a while; later ones take
seconds. `cargo test` runs the tests, including ones that open real windows
and shells without showing them.

## Roadmap

- Keep the language servers Wercode downloaded up to date
- Vim mode, bookmarks and a minimap in the editor
- Agents connected through the Agent Client Protocol, next to the ones in the terminal
- Signed Windows and macOS builds

Ideas and bug reports are welcome in [Issues](https://github.com/Werroks/Wercode/issues).

## Support Wercode

Wercode is free and open source, and it stays that way. If it saves you time and you
want to help it grow, you can send **USDT on the TRON network (TRC-20)** to:

```
TVb5SbxLo7kWetAw3Y7n4bdArfsfD1wLRX
```

Send only USDT on TRC-20 to this address; coins sent on another network are lost.
The same address is in Wercode under Settings → **Support Wercode**, with a button to copy it.
Thank you!

## License

Wercode is free and open source, under your choice of the
[MIT License](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE).
