//! The changes to one file, shown in a tab: what git would stage, what is
//! already staged, or what a commit of the history changed.

use std::path::{Path, PathBuf};

use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;
use wercode_core::{document, git, history};

pub struct DiffView {
    /// The project folder, where git is run.
    root: PathBuf,
    path: PathBuf,
    /// Shows what is in the index instead of what is in the working tree.
    staged: bool,
    /// Git does not track the file yet, so all of it counts as new.
    untracked: bool,
    /// A commit of the history (and its first parent) instead of the working tree.
    commit: Option<(String, Option<String>)>,
    state: Entity<DiffState>,
    /// What to say instead of a patch: still reading, nothing changed, or an error.
    note: Option<String>,
    task: Option<Task<()>>,
}

impl Focusable for DiffView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.focus_handle(cx)
    }
}

impl DiffView {
    pub fn new(root: PathBuf, path: PathBuf, staged: bool, untracked: bool, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| DiffState::new([], cx));
        let mut this = Self {
            root,
            path,
            staged,
            untracked,
            commit: None,
            state,
            note: Some("Reading changes\u{2026}".into()),
            task: None,
        };
        this.reload(cx);
        this
    }

    /// What a commit changed in a file, against its first parent.
    pub fn of_commit(root: PathBuf, path: PathBuf, hash: String, parent: Option<String>, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| DiffState::new([], cx));
        let mut this = Self {
            root,
            path,
            staged: false,
            untracked: false,
            commit: Some((hash, parent)),
            state,
            note: Some("Reading changes\u{2026}".into()),
            task: None,
        };
        this.reload(cx);
        this
    }

    /// Whether this tab shows the same side of the same file.
    pub fn shows(&self, path: &Path, staged: bool) -> bool {
        self.commit.is_none() && self.path == path && self.staged == staged
    }

    /// Whether this tab shows what this commit changed in this file.
    pub fn shows_commit(&self, path: &Path, hash: &str) -> bool {
        self.path == path && self.commit.as_ref().is_some_and(|(h, _)| h == hash)
    }

    pub fn is_commit(&self) -> bool {
        self.commit.is_some()
    }

    /// The name on the tab.
    pub fn label(&self) -> String {
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match &self.commit {
            Some((hash, _)) => format!("{name} ({})", &hash[..hash.len().min(7)]),
            None if self.staged => format!("{name} (staged)"),
            None => format!("{name} (changes)"),
        }
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The file was staged or committed since: follow where its changes are now.
    pub fn set_untracked(&mut self, untracked: bool) {
        self.untracked = untracked;
    }

    /// Asks git for the patch again.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let (root, path, staged, untracked) = (self.root.clone(), self.path.clone(), self.staged, self.untracked);
        let commit = self.commit.clone();
        let language = document::language(&self.path);
        self.task = Some(cx.spawn(async move |this, cx| {
            let patch = cx
                .background_executor()
                .spawn(async move {
                    match commit {
                        Some((hash, parent)) => history::commit_diff(&root, &hash, parent.as_deref(), &path),
                        None => git::diff(&root, &path, staged, untracked),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let files = patch.and_then(|patch| DiffFile::parse(&patch).map_err(|e| format!("{e:?}")));
                this.note = match &files {
                    Ok(files) if files.is_empty() => Some("No changes in this file.".into()),
                    Ok(_) => None,
                    Err(error) => Some(format!("Could not show the changes: {error}")),
                };
                let files: Vec<DiffFile> =
                    files.unwrap_or_default().into_iter().map(|file| file.with_language(language)).collect();
                this.state.update(cx, |state, cx| state.set_files(files, cx));
                cx.notify();
            });
        }));
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = match &self.note {
            Some(note) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(note.clone())
                .into_any_element(),
            None => Diff::new(&self.state).size_full().into_any_element(),
        };
        div().id("diff-view").size_full().bg(theme.background).child(body)
    }
}

#[cfg(test)]
impl DiffView {
    /// Lines added and removed, once the patch has been read.
    pub fn counts(&self, cx: &App) -> Option<(usize, usize)> {
        let files = self.state.read(cx).files();
        (self.note.is_none()).then(|| (files.iter().map(|f| f.additions()).sum(), files.iter().map(|f| f.deletions()).sum()))
    }

    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
}
