# Wercode

A desktop terminal with a file tree, git and (later) an editor next to it. Rust, GPUI.

## Rules

- The interface is English. Every string the user sees goes in English; a Russian
  translation comes later as a second language, so keep strings easy to find
  (no text assembled from fragments).
- This repository is public. Never commit anything that identifies the author or
  the machine: real names, e-mail addresses, home-directory paths, hostnames,
  screenshots showing a prompt with a user name, tokens, local config. Use
  placeholders such as `C:\Users\me\project` in tests and docs. Commit with a
  GitHub noreply address, not a personal one.
- The app must build and run on Windows, macOS and Linux. Platform code sits
  behind `cfg`, with an implementation (or an honest fallback) for each.
- Plain Ctrl keys belong to the shell. App shortcuts use Ctrl+Shift.

## Layout

- `crates/core` — no window, tested without a GPU: settings, state, themes,
  shell lookup, PTY terminal, links, git status, file listing, reading and
  writing documents, project search, watching the folder for changes.
- `crates/app` — the `wercode` binary: `workspace.rs` (root view, title, side
  panel, status bar, palette, welcome), `term/` (tabs, splits, terminal view),
  `panel/` (file tree, search, git panel), `editor.rs` (a file in a tab),
  `settings.rs` (the settings screen), `assets.rs` (bundled icons), `theme.rs`.
- `assets/themes/*.toml` — built-in colour schemes, compiled in.
- `docs/` — planning notes: `features.csv`, `build-log.md`, `architecture.md`.
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
- After finishing a feature, update `docs/features.csv` and
  `docs/build-log.md`.
