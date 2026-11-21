//! Fetching language servers into Wercode's own folder, the way Zed does:
//! a program from a project's GitHub releases, or npm packages run with a
//! Node.js that Wercode fetches too. Nothing needs npm, rustup or a package
//! manager on the computer.
//!
//! Each server gets a folder of its own under [`servers_dir`], with a small
//! file saying how to run it. A server is put together in a folder next to
//! that one and moved into place only when it is complete.
//!
//! Each version sits in a folder of its own inside the server's, and the
//! small file names the one in use. An update ([`update_all`]) adds a new
//! version beside the old one, which a running server may still be using;
//! versions nothing names any more go the next time Wercode starts
//! ([`prune`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::i18n::{t, tf};
use crate::update;

/// Where a server comes from when Wercode fetches it.
#[derive(Debug, Clone)]
pub enum Download {
    /// The latest release of a GitHub project: the file for this computer.
    GitHub {
        repo: &'static str,
        /// The file's name for an OS and an architecture, as Rust names
        /// them (`windows`, `x86_64`); `None` where there is none.
        asset: fn(&str, &str) -> Option<Asset>,
        /// The program inside, without `.exe`.
        binary: &'static str,
    },
    /// Packages from npm. The program `bin` of `package` is the server.
    Npm { packages: &'static [&'static str], package: &'static str, bin: &'static str },
}

/// How a release names the file for this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asset {
    /// The same name in every release.
    Exact(String),
    /// A name that starts and ends so, with the version between; the first
    /// of these pairs a release has a file for.
    Around(Vec<(String, String)>),
}

/// How to run a fetched server.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Installed {
    pub program: PathBuf,
    /// Arguments before the server's own: the script, for one run by Node.
    #[serde(default)]
    pub args: Vec<String>,
    /// The release or the package versions fetched; `None` from Wercode 0.6,
    /// which did not write it down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// When a newer version was last looked for, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<u64>,
}

/// The file in a server's folder that says how to run it.
const MARK: &str = "server.json";
/// A server's own name for the Node.js the npm servers run with.
const NODE: &str = "node";
/// How long a server goes before Wercode looks for a newer version of it.
const CHECK_EVERY: u64 = 24 * 60 * 60;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

/// The folder servers are fetched into.
pub fn servers_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "wercode").map(|d| d.data_local_dir().join("servers"))
}

/// How to run the server fetched into `dir` as `name`, if it is there.
pub fn installed_in(dir: &Path, name: &str) -> Option<Installed> {
    let text = std::fs::read_to_string(dir.join(name).join(MARK)).ok()?;
    let installed: Installed = serde_json::from_str(&text).ok()?;
    let complete = installed.program.is_file() && installed.args.iter().all(|arg| !arg.starts_with(&*dir.to_string_lossy()) || Path::new(arg).exists());
    complete.then_some(installed)
}

/// How to run a server Wercode fetched, if it did.
pub fn installed(name: &str) -> Option<Installed> {
    installed_in(&servers_dir()?, name)
}

/// Fetches a server into Wercode's folder. Blocks for as long as the
/// download takes; call it off the UI thread.
pub fn install(name: &str, download: &Download) -> Result<Installed, String> {
    let dir = servers_dir().ok_or_else(|| t("There is no folder for Wercode's files on this computer.").to_string())?;
    install_into(&dir, name, download)
}

pub fn install_into(dir: &Path, name: &str, download: &Download) -> Result<Installed, String> {
    match download {
        Download::GitHub { repo, asset, binary } => {
            let asset = asset(std::env::consts::OS, std::env::consts::ARCH)
                .ok_or_else(|| t("There is no download of this language server for this computer.").to_string())?;
            place(dir, name, |into| from_github(repo, &asset, binary, into))
        }
        Download::Npm { packages, package, bin } => {
            let node = match installed_in(dir, NODE) {
                Some(node) => node,
                None => place(dir, NODE, from_nodejs)?,
            };
            place(dir, name, |into| from_npm(&node, packages, package, bin, into))
        }
    }
}

fn make_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| tf("could not make {folder}: {error}", &[("folder", &dir.display()), ("error", &e)]))
}

/// The name of the folder a version is kept in: the version, made safe for
/// a file name, and the time, so a version fetched again gets a new folder
/// while the old one may still be running.
fn version_folder(version: Option<&str>, now: u64) -> String {
    let version: String = version
        .unwrap_or("unknown")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .take(60)
        .collect();
    format!("{version}-{now}")
}

/// Writes down how to run the server in `dir/name`: into a file beside the
/// old one first, so a reader never finds half of it.
fn write_mark(dir: &Path, name: &str, installed: &Installed) -> Result<(), String> {
    let home = dir.join(name);
    let text = serde_json::to_string_pretty(installed).map_err(|e| e.to_string())?;
    let fresh = home.join(format!("{MARK}.{}", std::process::id()));
    std::fs::write(&fresh, text).map_err(|e| e.to_string())?;
    std::fs::rename(&fresh, home.join(MARK)).map_err(|e| {
        let _ = std::fs::remove_file(&fresh);
        e.to_string()
    })
}

/// What `dir/name` says about its server, complete or not.
fn read_mark(dir: &Path, name: &str) -> Option<Installed> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(name).join(MARK)).ok()?).ok()
}

/// Puts a server together in a folder of its own next to `dir/name`, then
/// moves it into a folder for its version inside `dir/name` and writes
/// down how to run it. Older versions stay until [`prune`].
fn place(dir: &Path, name: &str, fill: impl FnOnce(&Path) -> Result<Installed, String>) -> Result<Installed, String> {
    let home = dir.join(name);
    let staging = dir.join(format!(".{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    make_dir(&staging)?;
    let filled = fill(&staging).and_then(|built| make_dir(&home).map(|()| built));
    let built = match filled {
        Ok(built) => built,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    let now = now();
    let target = home.join(version_folder(built.version.as_deref(), now));
    let _ = std::fs::remove_dir_all(&target);
    if let Err(error) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(tf("could not make {folder}: {error}", &[("folder", &target.display()), ("error", &error)]));
    }
    // Paths into the folder move with it.
    let (from, to) = (staging.to_string_lossy().into_owned(), target.to_string_lossy().into_owned());
    let moved = |path: &str| match path.strip_prefix(&from) {
        Some(rest) => format!("{to}{rest}"),
        None => path.to_string(),
    };
    let installed = Installed {
        program: PathBuf::from(moved(&built.program.to_string_lossy())),
        args: built.args.iter().map(|arg| moved(arg)).collect(),
        version: built.version,
        checked: Some(now),
    };
    write_mark(dir, name, &installed)?;
    Ok(installed)
}

/// Deletes what no server in `dir` runs from any more: the versions an
/// update replaced, and the files of servers fetched by Wercode 0.6, which
/// lay right in the server's folder. Call it before any server starts, as
/// Wercode starts: a server of this session may still run from an old
/// version. What cannot be deleted now (a program still running in another
/// Wercode) is tried again next time.
pub fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut servers = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        // A folder whose file cannot be read is left alone: nothing in it is known to be unused.
        if let Some(installed) = read_mark(dir, &name) {
            servers.push((name, installed));
        }
    }
    let used: Vec<PathBuf> = servers
        .iter()
        .flat_map(|(_, installed)| std::iter::once(installed.program.clone()).chain(installed.args.iter().map(PathBuf::from)))
        .filter(|path| path.starts_with(dir))
        .collect();
    for (name, _) in &servers {
        let Ok(entries) = std::fs::read_dir(dir.join(name)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name() == MARK || used.iter().any(|used| used.starts_with(&path)) {
                continue;
            }
            let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        }
    }
}

/// Whether it is time to look for a newer version of a server.
fn due(installed: &Installed, now: u64) -> bool {
    installed.checked.is_none_or(|checked| now.saturating_sub(checked) >= CHECK_EVERY)
}

/// Looks for a newer version of the server in `dir/name` once it is due:
/// `latest` says which version is newest, `fetch` fetches it. `Ok(true)`
/// when a new version was put in place.
fn update_with(
    dir: &Path,
    name: &str,
    now: u64,
    latest: impl FnOnce() -> Result<String, String>,
    fetch: impl FnOnce() -> Result<Installed, String>,
) -> Result<bool, String> {
    let Some(installed) = installed_in(dir, name) else { return Ok(false) };
    if !due(&installed, now) {
        return Ok(false);
    }
    let latest = latest()?;
    if installed.version.as_deref() == Some(latest.as_str()) {
        write_mark(dir, name, &Installed { checked: Some(now), ..installed })?;
        return Ok(false);
    }
    fetch()?;
    Ok(true)
}

/// Points the servers that ran with the Node.js at `old` at the one at
/// `new`, so the old Node.js can go.
fn move_to_node(dir: &Path, old: &Path, new: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == NODE || name.starts_with('.') {
            continue;
        }
        if let Some(installed) = read_mark(dir, &name).filter(|installed| installed.program == old) {
            let _ = write_mark(dir, &name, &Installed { program: new.to_path_buf(), ..installed });
        }
    }
}

/// Fetches newer versions of the servers Wercode fetched before, each at
/// most once a day; their next start uses them. Returns what was updated,
/// or could not be. Blocks for as long as the downloads take.
pub fn update_all(servers: &[(String, Download)]) -> Vec<(String, Result<bool, String>)> {
    let Some(dir) = servers_dir() else { return Vec::new() };
    update_all_in(&dir, servers)
}

pub fn update_all_in(dir: &Path, servers: &[(String, Download)]) -> Vec<(String, Result<bool, String>)> {
    let now = now();
    let mut results = Vec::new();
    // Node.js first, so the npm servers fetched after it run with the new one.
    let runs_node = servers.iter().any(|(name, download)| matches!(download, Download::Npm { .. }) && installed_in(dir, name).is_some());
    if runs_node && let Some(old) = installed_in(dir, NODE) {
        let result = update_with(dir, NODE, now, latest_node, || place(dir, NODE, from_nodejs));
        if result == Ok(true)
            && let Some(new) = installed_in(dir, NODE)
        {
            move_to_node(dir, &old.program, &new.program);
        }
        results.push((NODE.to_string(), result));
    }
    for (name, download) in servers {
        if installed_in(dir, name).is_none() {
            continue;
        }
        let result = update_with(dir, name, now, || latest_version(download), || install_into(dir, name, download));
        results.push((name.clone(), result));
    }
    results
}

/// The newest version of a server, written the way [`Installed::version`] is.
fn latest_version(download: &Download) -> Result<String, String> {
    match download {
        Download::GitHub { repo, .. } => latest_release(repo).map(|release| release.tag_name),
        Download::Npm { packages, .. } => {
            let mut versions = Vec::new();
            for package in *packages {
                #[derive(Deserialize)]
                struct Latest {
                    version: String,
                }
                let url = format!("https://registry.npmjs.org/{}/latest", package.replace('/', "%2F"));
                let latest: Latest = serde_json::from_slice(&get(&url)?).map_err(|e| e.to_string())?;
                versions.push(format!("{package}@{}", latest.version));
            }
            Ok(versions.join(" "))
        }
    }
}

/// Downloads a file over HTTPS.
fn fetch(url: &str, file: &Path) -> Result<(), String> {
    update::run(update::curl().args(["--max-time", "900", "--output"]).arg(file).arg(url)).map(drop)
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    update::run(update::curl().args(["--max-time", "60"]).arg(url))
}

/// A project's latest release on GitHub.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseFile>,
}

#[derive(Deserialize)]
struct ReleaseFile {
    name: String,
    browser_download_url: String,
}

fn latest_release(repo: &str) -> Result<Release, String> {
    let body = get(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

/// The name and address of the file of `release` that `asset` names: for
/// [`Asset::Around`], the first pair a file matches.
fn release_file(release: &Release, asset: &Asset) -> Option<(String, String)> {
    let file = match asset {
        Asset::Exact(name) => release.assets.iter().find(|file| &file.name == name),
        Asset::Around(names) => names.iter().find_map(|(start, end)| {
            release.assets.iter().find(|file| file.name.starts_with(start.as_str()) && file.name.ends_with(end.as_str()))
        }),
    }?;
    Some((file.name.clone(), file.browser_download_url.clone()))
}

fn from_github(repo: &str, asset: &Asset, binary: &str, into: &Path) -> Result<Installed, String> {
    let release = latest_release(repo)?;
    let (name, url) = release_file(&release, asset)
        .ok_or_else(|| t("There is no download of this language server for this computer.").to_string())?;
    let archive = into.join(&name);
    fetch(&url, &archive)?;
    unpack(&archive, into, binary)?;
    let program = find_binary(into, binary).ok_or_else(|| tf("{file} has no {program} in it.", &[("file", &name), ("program", &binary)]))?;
    make_runnable(&program)?;
    Ok(Installed { program, version: Some(release.tag_name), ..Installed::default() })
}

/// The name of a program on this computer: with `.exe` on Windows.
fn program_name(binary: &str) -> String {
    if cfg!(windows) { format!("{binary}.exe") } else { binary.to_string() }
}

/// Unpacks a downloaded file into `into` and deletes it: a `.zip`, a
/// `.tar.gz`, or one program compressed alone as `.gz`, which is named
/// `binary`. Anything else is the program itself.
fn unpack(archive: &Path, into: &Path, binary: &str) -> Result<(), String> {
    let name = archive.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let failed = |e: &dyn std::fmt::Display| tf("{file} could not be unpacked: {error}", &[("file", &name), ("error", e)]);
    if name.ends_with(".zip") {
        let file = std::fs::File::open(archive).map_err(|e| failed(&e))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| failed(&e))?;
        zip.extract(into).map_err(|e| failed(&e))?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let mut tar = Command::new("tar");
        tar.arg("-xzf").arg(archive).arg("-C").arg(into).stdin(Stdio::null());
        hide(&mut tar);
        let out = tar.output().map_err(|e| failed(&e))?;
        if !out.status.success() {
            return Err(failed(&String::from_utf8_lossy(&out.stderr).trim()));
        }
    } else if name.ends_with(".gz") {
        let file = std::fs::File::open(archive).map_err(|e| failed(&e))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut out = std::fs::File::create(into.join(program_name(binary))).map_err(|e| failed(&e))?;
        std::io::copy(&mut decoder, &mut out).map_err(|e| failed(&e))?;
    } else {
        std::fs::copy(archive, into.join(program_name(binary))).map_err(|e| failed(&e))?;
    }
    let _ = std::fs::remove_file(archive);
    Ok(())
}

/// The program named `binary` in `dir` or the folders in it, the one
/// nearest the top first. Without one of that very name, a program whose
/// name starts so (`taplo-full.exe` for `taplo`).
fn find_binary(dir: &Path, binary: &str) -> Option<PathBuf> {
    let wanted = program_name(binary);
    let like = |name: &str| {
        name.starts_with(binary) && if cfg!(windows) { name.ends_with(".exe") } else { !name.contains('.') }
    };
    let mut level = vec![dir.to_path_buf()];
    for _ in 0..6 {
        let mut next = Vec::new();
        let mut close = None;
        for folder in &level {
            let Ok(entries) = std::fs::read_dir(folder) else { continue };
            let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            entries.sort();
            for path in entries {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if path.is_dir() {
                    next.push(path);
                } else if name == wanted {
                    return Some(path);
                } else if close.is_none() && like(&name) {
                    close = Some(path);
                }
            }
        }
        if close.is_some() {
            return close;
        }
        level = next;
    }
    None
}

#[cfg(unix)]
fn make_runnable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn make_runnable(_: &Path) -> Result<(), String> {
    Ok(())
}

fn hide(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: no console flashing up next to a GUI app.
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// The name nodejs.org gives its download for an OS and architecture, and
/// whether it is a `.zip`.
fn node_platform(os: &str, arch: &str) -> Option<(&'static str, bool)> {
    Some(match (os, arch) {
        ("windows", "x86_64") => ("win-x64", true),
        ("windows", "aarch64") => ("win-arm64", true),
        ("macos", "x86_64") => ("darwin-x64", false),
        ("macos", "aarch64") => ("darwin-arm64", false),
        ("linux", "x86_64") => ("linux-x64", false),
        ("linux", "aarch64") => ("linux-arm64", false),
        _ => return None,
    })
}

/// The newest long-term release of Node.js (`v22.12.0`).
fn latest_node() -> Result<String, String> {
    #[derive(Deserialize)]
    struct Version {
        version: String,
        lts: serde_json::Value,
    }
    let versions: Vec<Version> = serde_json::from_slice(&get("https://nodejs.org/dist/index.json")?).map_err(|e| e.to_string())?;
    versions
        .into_iter()
        .find(|v| v.lts.as_bool() != Some(false) && !v.lts.is_null())
        .map(|v| v.version)
        .ok_or_else(|| t("There is no Node.js for this computer.").to_string())
}

/// Fetches the newest long-term release of Node.js, checked against the
/// sums nodejs.org publishes, with the npm that comes with it.
fn from_nodejs(into: &Path) -> Result<Installed, String> {
    let (platform, zip) = node_platform(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| t("There is no Node.js for this computer.").to_string())?;
    let version = latest_node()?;
    let name = format!("node-{version}-{platform}.{}", if zip { "zip" } else { "tar.gz" });
    let base = format!("https://nodejs.org/dist/{version}");
    let sums = String::from_utf8_lossy(&get(&format!("{base}/SHASUMS256.txt"))?).into_owned();
    let sum = sums
        .lines()
        .find_map(|line| line.split_once("  ").filter(|(_, file)| file.trim() == name).map(|(sum, _)| sum.to_string()))
        .ok_or_else(|| t("the downloaded file is damaged (its checksum does not match)").to_string())?;
    let archive = into.join(&name);
    fetch(&format!("{base}/{name}"), &archive)?;
    update::verify(&archive, &sum)?;
    unpack(&archive, into, NODE)?;
    let top = into.join(format!("node-{version}-{platform}"));
    let (program, npm) = if cfg!(windows) {
        (top.join("node.exe"), top.join("node_modules").join("npm").join("bin").join("npm-cli.js"))
    } else {
        (top.join("bin").join("node"), top.join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js"))
    };
    if !program.is_file() || !npm.is_file() {
        return Err(tf("{file} has no {program} in it.", &[("file", &name), ("program", &"node")]));
    }
    Ok(Installed { program, args: vec![npm.to_string_lossy().into_owned()], version: Some(version), checked: None })
}

/// Where a package's `package.json` says its program `bin` is.
fn package_bin(package_json: &str, bin: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(package_json).ok()?;
    match value.get("bin")? {
        serde_json::Value::String(path) => Some(path.clone()),
        serde_json::Value::Object(map) => map.get(bin)?.as_str().map(str::to_string),
        _ => None,
    }
}

/// Installs npm packages into `into` with Wercode's Node.js and its npm.
fn from_npm(node: &Installed, packages: &[&str], package: &str, bin: &str, into: &Path) -> Result<Installed, String> {
    let npm = node.args.first().ok_or_else(|| t("npm was not found.").to_string())?;
    let mut command = Command::new(&node.program);
    command
        .arg(npm)
        .args(["install", "--no-audit", "--no-fund", "--no-save", "--loglevel=error", "--prefix"])
        .arg(into)
        .args(packages)
        .current_dir(into)
        .stdin(Stdio::null());
    // Install scripts that run `node` find this one.
    if let Some(folder) = node.program.parent() {
        let mut path = vec![folder.to_path_buf()];
        path.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
        if let Ok(joined) = std::env::join_paths(path) {
            command.env("PATH", joined);
        }
    }
    hide(&mut command);
    let out = command.output().map_err(|e| tf("npm could not start: {error}", &[("error", &e)]))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        let last: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).rev().take(3).collect();
        return Err(tf("npm could not install {package}: {error}", &[("package", &package), ("error", &last.into_iter().rev().collect::<Vec<_>>().join(" "))]));
    }
    let folder = into.join("node_modules").join(package);
    let script = std::fs::read_to_string(folder.join("package.json"))
        .ok()
        .and_then(|text| package_bin(&text, bin))
        .ok_or_else(|| tf("{file} has no {program} in it.", &[("file", &package), ("program", &bin)]))?;
    let script = folder.join(script);
    if !script.is_file() {
        return Err(tf("{file} has no {program} in it.", &[("file", &package), ("program", &bin)]));
    }
    // The versions npm chose, written the way `latest_version` asks for them.
    let versions: Vec<String> = packages
        .iter()
        .map(|name| {
            let text = std::fs::read_to_string(into.join("node_modules").join(name).join("package.json")).unwrap_or_default();
            let version = package_version(&text).unwrap_or_default();
            format!("{name}@{version}")
        })
        .collect();
    Ok(Installed {
        program: node.program.clone(),
        args: vec![script.to_string_lossy().into_owned()],
        version: Some(versions.join(" ")),
        checked: None,
    })
}

/// The version a package's `package.json` gives.
fn package_version(package_json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(package_json).ok()?;
    value.get("version")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wercode-download-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_server_is_put_in_place_whole() {
        let dir = temp("place");
        assert_eq!(installed_in(&dir, "tool"), None);
        // A failure leaves nothing behind.
        assert!(place(&dir, "tool", |_| Err("no".into())).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        let installed = place(&dir, "tool", |into| {
            std::fs::create_dir_all(into.join("bin")).unwrap();
            std::fs::write(into.join("bin").join("tool"), "").unwrap();
            std::fs::write(into.join("main.js"), "").unwrap();
            Ok(Installed {
                program: into.join("bin").join("tool"),
                args: vec![into.join("main.js").to_string_lossy().into_owned(), "--stdio".into()],
                version: Some("1.0".into()),
                checked: None,
            })
        })
        .unwrap();
        let version = installed.program.parent().unwrap().parent().unwrap().to_path_buf();
        assert_eq!(version.parent(), Some(dir.join("tool").as_path()));
        assert!(version.file_name().unwrap().to_string_lossy().starts_with("1.0-"));
        assert_eq!(installed.program, version.join("bin").join("tool"));
        assert_eq!(installed.args, [version.join("main.js").to_string_lossy().into_owned(), "--stdio".into()]);
        assert_eq!(installed.version.as_deref(), Some("1.0"));
        assert!(installed.checked.is_some());
        assert_eq!(installed_in(&dir, "tool"), Some(installed));
        // A program that is gone counts as not installed.
        std::fs::remove_file(version.join("bin").join("tool")).unwrap();
        assert_eq!(installed_in(&dir, "tool"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Puts a fake server `name` at `version` into `dir`: a program, and a
    /// script when `node` runs it.
    fn fake_server(dir: &Path, name: &str, version: &str, node: Option<&Path>) -> Installed {
        place(dir, name, |into| {
            std::fs::write(into.join("server"), version).unwrap();
            Ok(match node {
                Some(node) => Installed {
                    program: node.to_path_buf(),
                    args: vec![into.join("server").to_string_lossy().into_owned()],
                    version: Some(version.into()),
                    checked: None,
                },
                None => Installed { program: into.join("server"), version: Some(version.into()), ..Installed::default() },
            })
        })
        .unwrap()
    }

    #[test]
    fn an_update_goes_beside_the_old_version_which_goes_at_the_next_start() {
        let dir = temp("update");
        let old = fake_server(&dir, "tool", "1.0", None);
        // Not due yet: nothing is asked.
        let now = old.checked.unwrap() + 60;
        let asked = std::cell::Cell::new(false);
        let latest = || {
            asked.set(true);
            Ok("2.0".to_string())
        };
        assert_eq!(update_with(&dir, "tool", now, latest, || unreachable!()), Ok(false));
        assert!(!asked.get());
        // A day later the same version only notes the time.
        let later = now + CHECK_EVERY;
        assert_eq!(update_with(&dir, "tool", later, || Ok("1.0".into()), || unreachable!()), Ok(false));
        assert_eq!(installed_in(&dir, "tool").unwrap().checked, Some(later));
        // A new version goes into a folder of its own; the old one stays while it may run.
        let much_later = later + CHECK_EVERY;
        let mut new = None;
        let fetched = update_with(&dir, "tool", much_later, || Ok("2.0".into()), || {
            new = Some(fake_server(&dir, "tool", "2.0", None));
            Ok(new.clone().unwrap())
        });
        assert_eq!(fetched, Ok(true));
        let new = new.unwrap();
        assert_eq!(installed_in(&dir, "tool"), Some(new.clone()));
        assert!(old.program.is_file());
        // Failing to ask leaves everything as it was.
        assert!(update_with(&dir, "tool", much_later + 2 * CHECK_EVERY, || Err("offline".into()), || unreachable!()).is_err());
        assert_eq!(installed_in(&dir, "tool"), Some(new.clone()));
        // The next start deletes the old version.
        prune(&dir);
        assert!(!old.program.parent().unwrap().exists());
        assert!(new.program.is_file());
        assert_eq!(installed_in(&dir, "tool"), Some(new));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn servers_from_wercode_0_6_are_updated_and_their_old_files_cleared() {
        let dir = temp("old-layout");
        // 0.6 put the files right in the server's folder and wrote no version.
        let home = dir.join("tool");
        std::fs::create_dir_all(home.join("lib")).unwrap();
        std::fs::write(home.join("tool.exe"), "").unwrap();
        std::fs::write(home.join(MARK), serde_json::json!({ "program": home.join("tool.exe") }).to_string()).unwrap();
        let before = installed_in(&dir, "tool").unwrap();
        assert_eq!((before.version, before.checked), (None, None));
        // Pruning keeps what the file names.
        prune(&dir);
        assert!(home.join("tool.exe").is_file());
        assert_eq!(update_with(&dir, "tool", now(), || Ok("2.0".into()), || Ok(fake_server(&dir, "tool", "2.0", None))), Ok(true));
        prune(&dir);
        assert!(!home.join("tool.exe").exists() && !home.join("lib").exists());
        assert_eq!(installed_in(&dir, "tool").unwrap().version.as_deref(), Some("2.0"));
        // A folder whose file cannot be read is left alone.
        std::fs::create_dir_all(dir.join("broken").join("stuff")).unwrap();
        std::fs::write(dir.join("broken").join(MARK), "not json").unwrap();
        prune(&dir);
        assert!(dir.join("broken").join("stuff").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn npm_servers_move_to_a_new_node_and_the_old_one_goes() {
        let dir = temp("node");
        let old_node = fake_server(&dir, NODE, "v22.1.0", None);
        let server = fake_server(&dir, "script", "script@1.0", Some(&old_node.program));
        let new_node = fake_server(&dir, NODE, "v22.2.0", None);
        // The old Node.js stays while a server runs with it.
        prune(&dir);
        assert!(old_node.program.is_file());
        move_to_node(&dir, &old_node.program, &new_node.program);
        let moved = installed_in(&dir, "script").unwrap();
        assert_eq!(moved.program, new_node.program);
        assert_eq!((moved.args, moved.version), (server.args, server.version));
        prune(&dir);
        assert!(!old_node.program.exists());
        assert!(new_node.program.is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn version_folders_are_safe_names() {
        assert_eq!(version_folder(Some("2026-11-17"), 5), "2026-11-17-5");
        assert_eq!(version_folder(Some("typescript-language-server@4.3.3 typescript@5.6.3"), 5), "typescript-language-server_4.3.3_typescript_5.6.3-5");
        assert_eq!(version_folder(None, 7), "unknown-7");
        assert_eq!(package_version(r#"{"name":"x","version":"1.2.3"}"#).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn downloads_are_unpacked_and_their_program_found() {
        let dir = temp("unpack");
        let exe = program_name("tool");
        // A zip with the program a few folders down.
        let zip_path = dir.join("tool-1.0.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file(format!("tool_1.0/bin/{exe}"), options).unwrap();
            zip.write_all(b"program").unwrap();
            zip.start_file("tool_1.0/README", options).unwrap();
            zip.finish().unwrap();
        }
        let into = dir.join("a");
        std::fs::create_dir_all(&into).unwrap();
        unpack(&zip_path, &into, "tool").unwrap();
        assert!(!zip_path.exists());
        let found = find_binary(&into, "tool").unwrap();
        assert_eq!(found, into.join("tool_1.0").join("bin").join(&exe));
        assert_eq!(std::fs::read(&found).unwrap(), b"program");

        // One program compressed alone gets the program's name.
        let gz_path = dir.join("tool-x86_64-unknown-linux-gnu.gz");
        let mut gz = flate2::write::GzEncoder::new(std::fs::File::create(&gz_path).unwrap(), flate2::Compression::default());
        gz.write_all(b"alone").unwrap();
        gz.finish().unwrap();
        let into = dir.join("b");
        std::fs::create_dir_all(&into).unwrap();
        unpack(&gz_path, &into, "tool").unwrap();
        assert_eq!(std::fs::read(into.join(&exe)).unwrap(), b"alone");
        assert_eq!(find_binary(&into, "missing"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_are_named_for_the_computer() {
        let rust = |os, arch| crate::lsp::builtin().into_iter().find(|s| s.name == "rust-analyzer").and_then(|s| match s.download {
            Some(Download::GitHub { asset, .. }) => asset(os, arch),
            _ => None,
        });
        assert_eq!(rust("windows", "x86_64"), Some(Asset::Exact("rust-analyzer-x86_64-pc-windows-msvc.zip".into())));
        assert_eq!(rust("macos", "aarch64"), Some(Asset::Exact("rust-analyzer-aarch64-apple-darwin.gz".into())));
        assert_eq!(rust("linux", "x86_64"), Some(Asset::Exact("rust-analyzer-x86_64-unknown-linux-gnu.gz".into())));
        assert_eq!(rust("freebsd", "x86_64"), None);
        assert_eq!(node_platform("macos", "aarch64"), Some(("darwin-arm64", false)));
        assert_eq!(node_platform("windows", "x86_64"), Some(("win-x64", true)));
        assert_eq!(package_bin(r#"{"bin":{"a":"lib/a.js","b":"b.js"}}"#, "b").as_deref(), Some("b.js"));
        assert_eq!(package_bin(r#"{"bin":"cli.js"}"#, "anything").as_deref(), Some("cli.js"));
        assert_eq!(package_bin(r#"{"name":"x"}"#, "x"), None);
        // Every server Wercode can fetch has a file for the common computers.
        for spec in crate::lsp::builtin() {
            if let Some(Download::GitHub { asset, .. }) = spec.download {
                for (os, arch) in [("windows", "x86_64"), ("linux", "x86_64"), ("macos", "aarch64")] {
                    assert!(asset(os, arch).is_some(), "{} on {os} {arch}", spec.name);
                }
            }
        }
    }

    /// Fetches real servers from the internet: `cargo test -p wercode-core -- --ignored downloads`.
    #[test]
    #[ignore]
    fn real_downloads() {
        let dir = temp("real");
        for spec in crate::lsp::builtin() {
            let Some(download) = &spec.download else { continue };
            if !["rust-analyzer", "taplo", "clangd", "ruff", "yaml-language-server"].contains(&spec.name.as_str()) {
                continue;
            }
            let installed = install_into(&dir, &spec.name, download).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            let mut run = Command::new(&installed.program);
            run.args(&installed.args).arg("--version");
            let out = run.output().unwrap();
            println!("{} -> {:?}: {}", spec.name, installed, String::from_utf8_lossy(&out.stdout).trim());
            assert_eq!(installed_in(&dir, &spec.name), Some(installed.clone()));
            // The newest version is written the way the fetched one is: a day later nothing is fetched again.
            let later = installed.checked.unwrap() + CHECK_EVERY;
            let again = update_with(&dir, &spec.name, later, || latest_version(download), || panic!("{} fetched again", spec.name));
            assert_eq!(again, Ok(false), "{}", spec.name);
        }
        let later = installed_in(&dir, NODE).unwrap().checked.unwrap() + CHECK_EVERY;
        assert_eq!(update_with(&dir, NODE, later, latest_node, || panic!("Node.js fetched again")), Ok(false));
        // taplo is started as `taplo lsp stdio`; the build fetched must have it.
        let taplo = installed_in(&dir, "taplo").unwrap();
        let out = Command::new(&taplo.program).args(["lsp", "--help"]).output().unwrap();
        assert!(out.status.success(), "taplo has no lsp: {}", String::from_utf8_lossy(&out.stderr));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
