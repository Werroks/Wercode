# Wercode

A desktop terminal with a file tree, git and (later) an editor next to it. Rust, GPUI.

## Rules

- The interface is written in English and translated into Russian, German,
  Spanish and Chinese (`language` in the settings; "auto" follows the
  computer). Every text the user sees goes through `t("…")`, `tf("… {name}", …)`
  or `tn(n, "{n} file", "{n} files", …)` from `core::i18n`, as one whole
  sentence (no text assembled from fragments), and gets a line in each of
  `crates/core/locales/*.toml`. A core test fails on a missing or stale
  translation or a changed `{hole}`.
- This repository is public. Never commit anything that identifies the author or
  the machine: real names, e-mail addresses, home-directory paths, hostnames,
  screenshots showing a prompt with a user name, tokens, local config. Use
  placeholders such as `C:\Users\me\project` in tests and docs. Commit with a
  GitHub noreply address, not a personal one.
- The app must build and run on Windows, macOS and Linux. Platform code sits
  behind `cfg`, with an implementation (or an honest fallback) for each.
- Plain Ctrl keys belong to the shell. App shortcuts use Ctrl+Shift.

## Layout

- `crates/core` — no window, tested without a GPU: settings, state, themes, tasks,
  which lines of a file changed (`linediff`),
  shell lookup, PTY terminal, pictures pasted into it (`pasted`), links, git
  status, file listing, finding a file
  by name, reading and
  writing documents, project search, watching the folder for changes,
  dropping what the program that started Wercode left in its environment,
  commit history and its graph (`history`), every change cut into hunks
  and single hunks staged or thrown away (`changes`), updates (`update`), the
  languages of the interface (`i18n`, with `locales/`), language servers
  for code checks (`lsp`: finding and running them, fetching them into
  Wercode's own folder (`lsp::download`), the protocol, what they report), which program reads a terminal's keys (`process::key_reader`).
- `crates/app` — the `wercode` binary: `workspace.rs` (root view, title, side
  panel, status bar, palette, welcome), `term/` (tabs, splits, terminal view),
  `panel/` (file tree, search, git panel, `graph.rs` for the history),
  `editor.rs` (a file in a tab), `update.rs` (checking and installing updates),
  `lsp.rs` (the shared language servers), `panel/problems.rs` (their list),
  `diff.rs` (the changes to a file in a tab), `changes.rs` (every change
  in one tab, with buttons for each hunk),
  `settings.rs` (the settings screen), `keymap.rs` (every shortcut, and the
  user's own from `[keys]`), `assets.rs` (bundled icons), `theme.rs`.
- `assets/themes/*.toml` — built-in colour schemes, compiled in.
- `docs/` — planning notes: `features.csv`, `build-log.md`, `architecture.md`.
- `packaging/` — the Windows installer script, the macOS bundle and the Linux
  archive, and `manifest.py` for `latest.json`. `.github/workflows/release.yml`
  runs them for a tag `v<version>`; the tag must match `Cargo.toml`.
- `tools/drive.ps1` — Windows-only helper for checking the running window. Not shipped.

## Commands

- `cargo run -- <folder>` runs the app; without a folder it reopens the last
  project or shows the welcome screen.
- `cargo test` runs everything. App tests open headless windows and real shells.
- `tools/drive.ps1` clicks, types and screenshots the running window on
  Windows without taking the keyboard focus. Save screenshots outside the
  repository: they show local paths.

## Things that bite

- An icon from `gpui_kit::assets::IconName` that is not in the component
  library's default set draws as nothing unless it is listed in
  `crates/app/src/assets.rs`. The test there checks the ones in use.
- UI tests that start a shell must call `cx.executor().allow_parking()`, or the
  PTY reader thread makes the test scheduler fail the test at random.
- `Cargo.lock` pins `taffy` 0.15.0 and `unicode-properties` 0.1.3 for the GPUI
  test build. Do not `cargo update` them blindly.
- Git is read by running the system `git` (`core::git`). Keep it off the UI
  thread and keep `GIT_OPTIONAL_LOCKS=0`.
- The Windows icon is resource 1 of the executable (`crates/app/build.rs`);
  the window takes it from there, so keep that number.
- A test must never use `state::state_path()`: that is the user's real saved
  state. Windows get the path passed in; tests pass one in a temp folder.
- A test must not write the user's `settings.toml` either: the settings path
  lives in `AppState` and stays unset in tests unless one sets a temp file.
- An element a UI test clicks by id needs `.test_support()`, unless it is a
  library button.
- Plain Ctrl+S is bound only inside the editor (`EditorView` context).
- The centre has two layouts (`layout` in the settings): one row of tabs, or
  files above a terminal panel. Both use the same list of tabs in
  `term/area.rs`; anything that picks a tab must work in both.
- A key binding without a context outranks every binding with one (GPUI
  counts it as matching at the deepest level). The component library binds
  Ctrl+Shift+I to its inspector that way, so that key cannot go to a command
  inside the terminals.
- Codex and PowerShell on Windows read key presses from the console, not
  bytes, and read Ctrl+letter through the keyboard layout (Ctrl+J is a
  Cyrillic letter in Russian). ConPTY asks for whole key presses as it starts
  (`ESC[?9001h`, seen by `osc::Win32InputScanner`). Shift+Enter goes as a
  Shift+Enter press, except to Claude Code, which ignores Shift there and gets
  Ctrl+Enter (`input::new_line`, `process::key_reader`). Try any new key
  encoding on Claude Code, Codex and PowerShell, under a Russian layout too.
- Shortcuts are bound in `keymap.rs` only, so the user can change them; a new
  command gets a line there, not a `KeyBinding` of its own.
- In a test module `use super::*` next to `use gpui_kit::*` makes `#[test]`
  mean the library's macro; import the names you need instead.
- Wercode started from Claude Code (`cargo run` in its tools) inherits
  `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `NO_COLOR=1` and git settings
  meant for a program. `core::environment::drop_leftovers()` removes them as
  the first line of `main`; keep it before anything that starts a thread.
- `drive.ps1`'s `text:` (WM_CHAR) does not reach the current window. Type
  with `key:` and virtual key codes; switch the window to the English layout
  first (`WM_INPUTLANGCHANGEREQUEST` with `LoadKeyboardLayout("00000409")`),
  or the letters come out in the user's layout.
- Only builds with `WERCODE_RELEASE` set at compile time (the release
  workflow) update themselves. To release: raise `version` in `Cargo.toml`,
  commit, push, then push the tag `v<version>`.
- Tests that need a language server run the test binary itself as one
  (`core::lsp::fake`): a plain `#[test]` calls `fake::serve()` when an
  environment variable is set, and the spec starts the binary with
  `fake::test_args(<that test's path>)`. No real server is needed in CI.
- The editor component clears its underlines on every edit; `EditorView`
  puts the last known problems back once typing pauses.
- After finishing a feature, update `docs/features.csv` and
  `docs/build-log.md`.
- `lsp::download::tests::real_downloads` fetches real servers from the
  internet; it is `#[ignore]` and never runs in CI.
