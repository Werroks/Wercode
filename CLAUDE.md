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
  shell lookup, PTY terminal, links, git status, file listing.
- `crates/app` — the `wercode` binary: `workspace.rs` (root view, title, side
  panel, status bar, palette, welcome), `term/` (tabs, splits, terminal view),
  `panel/` (file tree), `assets.rs` (bundled icons), `theme.rs`.
- `assets/themes/*.toml` — built-in colour schemes, compiled in.
- `replica/` — planning notes (`features.csv`, `build-log.md`, `architecture.md`)
  and Windows-only helper scripts. Not shipped.

## Commands

- `cargo run -- <folder>` runs the app; without a folder it reopens the last
  project or shows the welcome screen.
- `cargo test` runs everything. App tests open headless windows and real shells.
- `replica/drive.ps1` clicks, types and screenshots the running window on
  Windows without taking the keyboard focus. Prefer it over `shot.ps1`, which
  types into whatever window is in front.

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
- After finishing a feature, update `replica/features.csv` and
  `replica/build-log.md`.
