//! The language servers every window shares, and what they report: the
//! editor underlines it, the status bar counts it, the problems panel lists it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::*;
use wercode_core::document;
use wercode_core::lsp::{self, Edit, Event, FileEdits, Servers, Spec};

use crate::theme::AppState;

actions!(lsp, [RestartLanguageServers]);

/// How often servers that were not installed are looked for again, so one
/// installed from the status bar starts by itself.
const RETRY: Duration = Duration::from_secs(10);
/// How long Wercode waits for the servers to stop as it quits.
const QUIT_WAIT: Duration = Duration::from_millis(1500);

pub enum CheckerEvent {
    /// What the servers say about this file changed.
    Problems(PathBuf),
    /// Changes a language server wants in a file an editor has open: a
    /// quick fix, a rename.
    Edits(PathBuf, Vec<Edit>),
}

/// A server Wercode is fetching into its own folder, or could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetching {
    Running,
    Failed(String),
}

pub struct Checker {
    servers: Servers,
    /// Servers being fetched, or whose fetch failed, by name.
    fetching: HashMap<String, Fetching>,
    _events: Task<()>,
    _retry: Task<()>,
}

impl EventEmitter<CheckerEvent> for Checker {}

impl Checker {
    pub fn servers(&self) -> &Servers {
        &self.servers
    }

    pub fn fetching(&self, name: &str) -> Option<&Fetching> {
        self.fetching.get(name)
    }

    /// Fetches a language server into Wercode's own folder and starts it
    /// for the files waiting for it.
    pub fn fetch(&mut self, name: String, cx: &mut Context<Self>) {
        if self.fetching.get(&name) == Some(&Fetching::Running) {
            return;
        }
        let Some(download) = self.servers.download_of(&name) else { return };
        self.fetching.insert(name.clone(), Fetching::Running);
        cx.notify();
        let servers = self.servers.clone();
        cx.spawn(async move |this, cx| {
            let fetched = name.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let result = lsp::download::install(&fetched, &download).map(drop);
                    if result.is_ok() {
                        servers.retry_missing();
                    }
                    result
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => drop(this.fetching.remove(&name)),
                    Err(error) => drop(this.fetching.insert(name, Fetching::Failed(error))),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

struct Shared(Entity<Checker>);

impl Global for Shared {}

/// The checker, once [`init`] has run.
pub fn checker(cx: &App) -> Option<Entity<Checker>> {
    cx.try_global::<Shared>().map(|shared| shared.0.clone())
}

pub fn init(cx: &mut App) {
    init_with(lsp::builtin(), cx);
}

/// Starts checking with the servers in `specs`; tests pass a server of their own.
pub fn init_with(specs: Vec<Spec>, cx: &mut App) {
    let (sender, mut events) = mpsc::unbounded();
    let servers = Servers::new(specs, move |event| {
        let _ = sender.unbounded_send(event);
    });
    servers.set_enabled(cx.global::<AppState>().settings.language_servers);
    let checker = cx.new(|cx| {
        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if let Event::Edit(files) = event {
                    cx.update(|cx| apply_file_edits(files, cx));
                    continue;
                }
                let alive = this.update(cx, |_, cx| {
                    if let Event::Problems(path) = event {
                        cx.emit(CheckerEvent::Problems(path));
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let looking = servers.clone();
        let retry = cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(RETRY).await;
                let servers = looking.clone();
                cx.background_executor().spawn(async move { servers.retry_missing() }).await;
            }
        });
        Checker { servers: servers.clone(), fetching: HashMap::new(), _events: events, _retry: retry }
    });
    cx.set_global(Shared(checker));

    let switched = servers.clone();
    cx.observe_global::<AppState>(move |cx| switched.set_enabled(cx.global::<AppState>().settings.language_servers))
        .detach();
    cx.on_app_quit(move |_| {
        let servers = servers.clone();
        async move { servers.stop_all(QUIT_WAIT) }
    })
    .detach();
}

/// Keeps a project's servers running while a window shows it.
pub fn project(root: &Path, cx: &App) -> Option<lsp::ProjectGuard> {
    checker(cx).map(|checker| checker.read(cx).servers.project(root))
}

/// Makes the changes a language server asked for: in the editors that have
/// a file open (where they can be undone), and on disk for the others.
pub fn apply_file_edits(files: Vec<FileEdits>, cx: &mut App) {
    let Some(checker) = checker(cx) else { return };
    let servers = checker.read(cx).servers.clone();
    let (open, closed): (Vec<FileEdits>, Vec<FileEdits>) = files.into_iter().partition(|file| servers.is_open(&file.path));
    checker.update(cx, |_, cx| {
        for file in open {
            cx.emit(CheckerEvent::Edits(file.path, file.edits));
        }
    });
    if !closed.is_empty() {
        cx.background_executor()
            .spawn(async move {
                for file in closed {
                    if let Err(error) = write_edits(&file) {
                        eprintln!("could not change {}: {error}", file.path.display());
                    }
                }
            })
            .detach();
    }
}

/// Changes a file on disk, keeping its line endings and byte-order mark.
fn write_edits(file: &FileEdits) -> Result<(), String> {
    let doc = document::load(&file.path).map_err(|e| e.message())?;
    let text = lsp::apply_edits(&doc.text, &file.edits);
    document::save(&file.path, &text, doc.format).map(drop)
}
