//! Keeps Wercode up to date: looks for a new release now and then, downloads
//! it in the background, and puts it in place when the app quits.

use std::time::Duration;

use gpui_kit::*;
use wercode_core::update::{self, Staged};

use crate::theme::AppState;

actions!(
    wercode,
    [
        CheckForUpdates,
        /// Closes every window the usual way (asking about unsaved files and
        /// running programs), then installs the update and starts again.
        RestartToUpdate,
        /// Sent to each window by [`RestartToUpdate`].
        CloseForUpdate
    ]
);

/// The first look comes a little after start, when the window is up.
const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(4 * 60 * 60);
/// How long the result of a check asked for by hand stays in the status bar.
const SHOW_RESULT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not looked yet.
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    /// Downloaded and checked; goes in when Wercode quits.
    Ready(String),
    Failed(String),
    /// A build from source: updates come with installed releases only.
    Off,
}

pub struct Updater {
    status: Status,
    staged: Option<Staged>,
    /// The person asked for this check, so its result is shown to them.
    asked: bool,
    /// Quitting was asked for to finish the update: start again after it.
    relaunch: bool,
    _schedule: Option<Task<()>>,
}

impl Global for Updater {}

pub fn init(cx: &mut App) {
    let enabled = update::enabled();
    update::mark_running();
    if enabled {
        cx.background_executor().spawn(async { update::clean_up() }).detach();
    }
    let schedule = enabled.then(|| {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            loop {
                cx.update(|cx| {
                    if cx.global::<AppState>().settings.auto_update {
                        check(false, cx);
                    }
                });
                cx.background_executor().timer(CHECK_EVERY).await;
            }
        })
    });
    cx.set_global(Updater {
        status: if enabled { Status::Idle } else { Status::Off },
        staged: None,
        asked: false,
        relaunch: false,
        _schedule: schedule,
    });
    cx.on_app_quit(|cx| {
        finish(cx);
        async {}
    })
    .detach();
}

pub fn status(cx: &App) -> Status {
    cx.try_global::<Updater>().map(|u| u.status.clone()).unwrap_or(Status::Off)
}

/// The status, if the status bar should show it: a download, an update
/// waiting for a restart, or the answer to a check asked for by hand.
pub fn status_bar(cx: &App) -> Option<Status> {
    let updater = cx.try_global::<Updater>()?;
    match &updater.status {
        Status::Downloading(_) | Status::Ready(_) => Some(updater.status.clone()),
        Status::Checking | Status::UpToDate | Status::Failed(_) if updater.asked => Some(updater.status.clone()),
        _ => None,
    }
}

fn set(status: Status, cx: &mut App) {
    cx.update_global::<Updater, _>(|updater, _| updater.status = status);
}

/// Looks for a newer release and downloads it. `asked` means the person
/// asked for it, so even "up to date" is worth telling.
pub fn check(asked: bool, cx: &mut App) {
    let Some(updater) = cx.try_global::<Updater>() else { return };
    if matches!(updater.status, Status::Off | Status::Checking | Status::Downloading(_) | Status::Ready(_)) {
        return;
    }
    cx.update_global::<Updater, _>(|updater, _| {
        updater.asked = asked;
        updater.status = Status::Checking;
    });
    cx.spawn(async move |cx| {
        let found = cx.background_executor().spawn(async { update::check() }).await;
        let release = match found {
            Ok(Some(release)) => release,
            Ok(None) => return finished(Status::UpToDate, cx).await,
            Err(error) => return finished(Status::Failed(error), cx).await,
        };
        let version = release.version.clone();
        cx.update(|cx| set(Status::Downloading(version.clone()), cx));
        match cx.background_executor().spawn(async move { update::download(&release) }).await {
            Ok(staged) => cx.update_global::<Updater, _>(|updater, _| {
                updater.staged = Some(staged);
                updater.status = Status::Ready(version);
            }),
            Err(error) => finished(Status::Failed(error), cx).await,
        }
    })
    .detach();
}

/// Shows the end of a check, and takes it off the status bar after a while.
async fn finished(status: Status, cx: &mut AsyncApp) {
    cx.update(|cx| set(status, cx));
    cx.background_executor().timer(SHOW_RESULT).await;
    cx.update_global::<Updater, _>(|updater, _| updater.asked = false);
}

/// Closes the windows to restart into the new version. A window that is
/// asked about unsaved files or running programs may say no; the update then
/// waits for the next quit.
pub fn restart(cx: &mut App) {
    if cx.try_global::<Updater>().is_none_or(|u| u.staged.is_none()) {
        return;
    }
    cx.update_global::<Updater, _>(|updater, _| updater.relaunch = true);
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, cx| window.dispatch_action(Box::new(CloseForUpdate), cx));
    }
}

/// A window said no to closing: no restart after all.
pub fn restart_cancelled(cx: &mut App) {
    if cx.has_global::<Updater>() {
        cx.update_global::<Updater, _>(|updater, _| updater.relaunch = false);
    }
}

/// Puts a downloaded update in place as the app quits.
fn finish(cx: &mut App) {
    let Some(updater) = cx.try_global::<Updater>() else { return };
    if let Some(staged) = &updater.staged
        && let Err(error) = update::apply(staged, updater.relaunch)
    {
        eprintln!("update not installed: {error}");
    }
}
