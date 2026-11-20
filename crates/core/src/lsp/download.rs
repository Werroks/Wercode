//! Fetching language servers into Wercode's own folder, the way Zed does:
//! a program from a project's GitHub releases, or npm packages run with a
//! Node.js that Wercode fetches too. Nothing needs npm, rustup or a package
//! manager on the computer.
//!
//! Each server gets a folder of its own under [`servers_dir`], with a small
//! file saying how to run it. A server is put together in a folder next to
//! that one and moved into place only when it is complete.

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub program: PathBuf,
    /// Arguments before the server's own: the script, for one run by Node.
    #[serde(default)]
    pub args: Vec<String>,
}

/// The file in a server's folder that says how to run it.
const MARK: &str = "server.json";
/// A server's own name for the Node.js the npm servers run with.
const NODE: &str = "node";

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

/// Puts a server together in a folder of its own next to `dir/name`, then
/// moves it there and writes down how to run it.
fn place(dir: &Path, name: &str, fill: impl FnOnce(&Path) -> Result<Installed, String>) -> Result<Installed, String> {
    let target = dir.join(name);
    let staging = dir.join(format!(".{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    make_dir(&staging)?;
    let filled = fill(&staging);
    let built = match filled {
        Ok(built) => built,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
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
    };
    let text = serde_json::to_string_pretty(&installed).map_err(|e| e.to_string())?;
    std::fs::write(target.join(MARK), text).map_err(|e| e.to_string())?;
    Ok(installed)
}

/// Downloads a file over HTTPS.
fn fetch(url: &str, file: &Path) -> Result<(), String> {
    update::run(update::curl().args(["--max-time", "900", "--output"]).arg(file).arg(url)).map(drop)
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    update::run(update::curl().args(["--max-time", "60"]).arg(url))
}

/// The address of a file of a project's latest release whose name starts
/// and ends as one of `names` says, the first that matches, asked of GitHub.
fn latest_asset(repo: &str, names: &[(String, String)]) -> Result<(String, String), String> {
    #[derive(Deserialize)]
    struct Release {
        assets: Vec<File>,
    }
    #[derive(Deserialize)]
    struct File {
        name: String,
        browser_download_url: String,
    }
    let body = get(&format!("https://api.github.com/repos/{repo}/releases/latest"))?;
    let release: Release = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    names
        .iter()
        .find_map(|(start, end)| release.assets.iter().find(|file| file.name.starts_with(start.as_str()) && file.name.ends_with(end.as_str())))
        .map(|file| (file.name.clone(), file.browser_download_url.clone()))
        .ok_or_else(|| t("There is no download of this language server for this computer.").to_string())
}

fn from_github(repo: &str, asset: &Asset, binary: &str, into: &Path) -> Result<Installed, String> {
    let (name, url) = match asset {
        Asset::Exact(name) => (name.clone(), format!("https://github.com/{repo}/releases/latest/download/{name}")),
        Asset::Around(names) => latest_asset(repo, names)?,
    };
    let archive = into.join(&name);
    fetch(&url, &archive)?;
    unpack(&archive, into, binary)?;
    let program = find_binary(into, binary).ok_or_else(|| tf("{file} has no {program} in it.", &[("file", &name), ("program", &binary)]))?;
    make_runnable(&program)?;
    Ok(Installed { program, args: Vec::new() })
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

/// Fetches the newest long-term release of Node.js, checked against the
/// sums nodejs.org publishes, with the npm that comes with it.
fn from_nodejs(into: &Path) -> Result<Installed, String> {
    #[derive(Deserialize)]
    struct Version {
        version: String,
        lts: serde_json::Value,
    }
    let (platform, zip) = node_platform(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| t("There is no Node.js for this computer.").to_string())?;
    let versions: Vec<Version> = serde_json::from_slice(&get("https://nodejs.org/dist/index.json")?).map_err(|e| e.to_string())?;
    let version = versions
        .into_iter()
        .find(|v| v.lts.as_bool() != Some(false) && !v.lts.is_null())
        .map(|v| v.version)
        .ok_or_else(|| t("There is no Node.js for this computer.").to_string())?;
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
    Ok(Installed { program, args: vec![npm.to_string_lossy().into_owned()] })
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
    Ok(Installed { program: node.program.clone(), args: vec![script.to_string_lossy().into_owned()] })
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
            Ok(Installed { program: into.join("bin").join("tool"), args: vec![into.join("main.js").to_string_lossy().into_owned(), "--stdio".into()] })
        })
        .unwrap();
        assert_eq!(installed.program, dir.join("tool").join("bin").join("tool"));
        assert_eq!(installed.args, [dir.join("tool").join("main.js").to_string_lossy().into_owned(), "--stdio".into()]);
        assert_eq!(installed_in(&dir, "tool"), Some(installed));
        // A program that is gone counts as not installed.
        std::fs::remove_file(dir.join("tool").join("bin").join("tool")).unwrap();
        assert_eq!(installed_in(&dir, "tool"), None);
        std::fs::remove_dir_all(&dir).unwrap();
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
            assert_eq!(installed_in(&dir, &spec.name), Some(installed));
        }
    }
}
