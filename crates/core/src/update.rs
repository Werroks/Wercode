//! Updates: whether there is a newer release, fetching it, and putting it in
//! place when Wercode quits.
//!
//! Every release on GitHub carries `latest.json`: its version and, for each
//! platform, the file to download with its SHA-256. The newest release's copy
//! is at a fixed address. Downloads go through the system `curl` (part of
//! Windows 10 and later, macOS and nearly every Linux), so the app carries no
//! TLS code of its own and uses the system's certificates and proxy.
//!
//! How the new version goes in depends on the platform:
//! - Windows: the installer runs silently; it waits until Wercode has quit.
//! - macOS: the new `Wercode.app` takes the place of the old one.
//! - Linux: the new executable takes the place of the old one.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// GitHub sends this address to the manifest of the latest release.
pub const MANIFEST_URL: &str = "https://github.com/Werroks/Wercode/releases/latest/download/latest.json";
pub const RELEASES_URL: &str = "https://github.com/Werroks/Wercode/releases";

/// The version of this build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Only builds made by the release workflow update themselves. A build from
/// source would otherwise swap itself for a download.
pub fn enabled() -> bool {
    option_env!("WERCODE_RELEASE").is_some() && platform().is_some()
}

/// A release newer than this build, with the file for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub sha256: String,
    /// The release page, with what changed.
    pub notes: String,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    assets: BTreeMap<String, Asset>,
}

#[derive(Deserialize)]
struct Asset {
    url: String,
    sha256: String,
}

/// The name of this platform's file in the manifest.
pub fn platform() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else if cfg!(target_os = "macos") {
        Some("macos")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else {
        None
    }
}

/// `1.2.3` or `v1.2.3` as numbers. Anything else, such as a pre-release, is
/// not a version this offers.
fn numbers(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let mut out = [0; 3];
    for slot in &mut out {
        *slot = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (numbers(candidate), numbers(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// The release a manifest describes, if it is newer than `current` and has a
/// file for `platform`.
pub fn newer_release(manifest: &str, current: &str, platform: &str) -> Result<Option<Release>, String> {
    let manifest: Manifest =
        serde_json::from_str(manifest).map_err(|e| format!("the release manifest could not be read: {e}"))?;
    if !is_newer(&manifest.version, current) {
        return Ok(None);
    }
    let Some(asset) = manifest.assets.get(platform) else { return Ok(None) };
    Ok(Some(Release {
        version: manifest.version.trim_start_matches('v').to_string(),
        url: asset.url.clone(),
        sha256: asset.sha256.to_ascii_lowercase(),
        notes: manifest.notes,
    }))
}

fn curl() -> Command {
    let mut cmd = Command::new("curl");
    cmd.args(["--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--retry", "2"])
        .args(["--connect-timeout", "20"])
        .arg("--user-agent")
        .arg(format!("Wercode/{VERSION}"))
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: no console flashing up next to a GUI app.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

fn run(cmd: &mut Command) -> Result<Vec<u8>, String> {
    let out = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "curl was not found; it is needed to download updates".to_string(),
        _ => format!("curl could not start: {e}"),
    })?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().trim_start_matches("curl: ").to_string();
    Err(if said.is_empty() { "the download failed".to_string() } else { said })
}

/// Asks whether there is a release newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let platform = platform().ok_or("there are no updates for this platform")?;
    let body = run(curl().args(["--max-time", "60"]).arg(MANIFEST_URL))?;
    newer_release(&String::from_utf8_lossy(&body), VERSION, platform)
}

/// Fails unless the file's SHA-256 is `expected` (lower-case hex).
pub fn verify(file: &Path, expected: &str) -> Result<(), String> {
    let mut reader = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if actual == expected.to_ascii_lowercase() {
        Ok(())
    } else {
        Err("the downloaded file is damaged (its checksum does not match)".to_string())
    }
}

/// Where downloads wait until Wercode quits.
fn download_dir() -> PathBuf {
    std::env::temp_dir().join("wercode-update")
}

/// The name of the folder an update is unpacked into, next to what it replaces.
const STAGING: &str = ".wercode-update";

/// Removes what earlier updates left behind. Called at start, when nothing
/// is waiting to be put in place.
pub fn clean_up() {
    let _ = std::fs::remove_dir_all(download_dir());
    if let Ok(exe) = std::env::current_exe() {
        for place in [installed_bundle(&exe), Some(exe.clone())].into_iter().flatten() {
            if let Some(dir) = place.parent() {
                let _ = std::fs::remove_dir_all(dir.join(STAGING));
            }
        }
    }
}

/// An update that is downloaded, checked and ready to go in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staged {
    /// A Windows installer to run.
    Installer(PathBuf),
    /// A new `Wercode.app` (macOS), unpacked next to the one it replaces.
    Bundle { new: PathBuf, old: PathBuf },
    /// A new executable (Linux), unpacked next to the one it replaces.
    Executable { new: PathBuf, old: PathBuf },
}

/// Downloads a release, checks it and gets it ready to put in place.
pub fn download(release: &Release) -> Result<Staged, String> {
    let dir = download_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let name = release.url.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("wercode-update");
    let file = dir.join(name);
    run(curl().args(["--max-time", "1800", "--output"]).arg(&file).arg(&release.url))?;
    verify(&file, &release.sha256)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cfg!(windows) {
        Ok(Staged::Installer(file))
    } else if cfg!(target_os = "macos") {
        let bundle = installed_bundle(&exe).ok_or("Wercode is not running from Wercode.app")?;
        stage_bundle(&file, &bundle)
    } else {
        stage_executable(&file, &exe.canonicalize().unwrap_or(exe))
    }
}

/// `Wercode.app` when `exe` is `Wercode.app/Contents/MacOS/wercode`.
fn installed_bundle(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    bundle.extension().is_some_and(|e| e == "app").then(|| bundle.to_path_buf())
}

/// Unpacks a `.tar.gz` with the system `tar`, which Windows 10 has as well.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(into);
    std::fs::create_dir_all(into).map_err(|e| format!("could not make {}: {e}", into.display()))?;
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("tar could not start: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("the update could not be unpacked: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Unpacks a release archive holding `Wercode.app` next to the installed one.
/// The folder must be writable, since that is where the new bundle goes.
pub fn stage_bundle(archive: &Path, installed: &Path) -> Result<Staged, String> {
    let parent = installed.parent().ok_or("Wercode.app has no folder")?;
    let into = parent.join(STAGING);
    unpack(archive, &into)?;
    let new = into.join("Wercode.app");
    if !new.join("Contents").is_dir() {
        return Err("the update does not contain Wercode.app".into());
    }
    Ok(Staged::Bundle { new, old: installed.to_path_buf() })
}

/// Unpacks a release archive holding `wercode/wercode` next to the installed
/// executable.
pub fn stage_executable(archive: &Path, installed: &Path) -> Result<Staged, String> {
    let parent = installed.parent().ok_or("the executable has no folder")?;
    let into = parent.join(STAGING);
    unpack(archive, &into)?;
    let new = into.join("wercode").join("wercode");
    if !new.is_file() {
        return Err("the update does not contain the wercode executable".into());
    }
    Ok(Staged::Executable { new, old: installed.to_path_buf() })
}

/// Puts the update in place. Called as Wercode quits; with `relaunch` the
/// new version starts once it is in.
pub fn apply(staged: &Staged, relaunch: bool) -> Result<(), String> {
    match staged {
        Staged::Installer(file) => {
            let mut cmd = Command::new(file);
            cmd.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"]);
            if relaunch {
                cmd.arg("/RELAUNCH=1");
            }
            cmd.spawn().map(drop).map_err(|e| format!("the installer could not start: {e}"))
        }
        Staged::Bundle { new, old } => {
            let parked = old.with_file_name(".Wercode-old.app");
            let _ = std::fs::remove_dir_all(&parked);
            std::fs::rename(old, &parked).map_err(|e| format!("could not move {}: {e}", old.display()))?;
            if let Err(e) = std::fs::rename(new, old) {
                let _ = std::fs::rename(&parked, old);
                return Err(format!("could not put the new Wercode.app in place: {e}"));
            }
            let _ = std::fs::remove_dir_all(&parked);
            if relaunch {
                Command::new("open").arg("-n").arg(old).spawn().map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Staged::Executable { new, old } => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(new, std::fs::Permissions::from_mode(0o755));
            }
            // A rename replaces the file in one step; the running program keeps the old one open.
            std::fs::rename(new, old).map_err(|e| format!("could not replace {}: {e}", old.display()))?;
            if relaunch {
                Command::new(old).spawn().map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
}

/// Tells the Windows installer that Wercode is running, so that an update
/// started on the way out waits until it is gone.
#[cfg(windows)]
pub fn mark_running() {
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "WercodeRunning\0".encode_utf16().collect();
    // SAFETY: a NUL-terminated name and no security attributes. The handle is
    // never closed on purpose: the mutex must live as long as the process.
    unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
}

#[cfg(not(windows))]
pub fn mark_running() {}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "version": "0.3.0",
        "notes": "https://github.com/Werroks/Wercode/releases/tag/v0.3.0",
        "assets": {
            "windows-x86_64": { "url": "https://example.com/Wercode-Setup-0.3.0-x64.exe", "sha256": "ABC123" },
            "macos": { "url": "https://example.com/Wercode-0.3.0-macos.tar.gz", "sha256": "def456" }
        }
    }"#;

    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("0.3.0-beta", "0.2.0"), "pre-releases are not offered");
        assert!(!is_newer("0.3", "0.2.0"));
    }

    #[test]
    fn the_manifest_gives_this_platforms_file_when_it_is_newer() {
        let release = newer_release(MANIFEST, "0.2.5", "windows-x86_64").unwrap().unwrap();
        assert_eq!(release.version, "0.3.0");
        assert_eq!(release.url, "https://example.com/Wercode-Setup-0.3.0-x64.exe");
        assert_eq!(release.sha256, "abc123");
        assert!(release.notes.ends_with("v0.3.0"));
        assert_eq!(newer_release(MANIFEST, "0.3.0", "windows-x86_64").unwrap(), None);
        assert_eq!(newer_release(MANIFEST, "0.2.5", "linux-x86_64").unwrap(), None);
        assert!(newer_release("<html>", "0.2.5", "macos").is_err());
    }

    #[test]
    fn a_download_must_match_its_checksum() {
        let dir = std::env::temp_dir().join(format!("wercode-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data");
        std::fs::write(&file, "hello").unwrap();
        let hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(verify(&file, hello), Ok(()));
        assert_eq!(verify(&file, &hello.to_uppercase()), Ok(()));
        assert!(verify(&file, &hello.replace('2', "3")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn pack(dir: &Path, top: &str) -> PathBuf {
        let archive = dir.join("release.tar.gz");
        let status = Command::new("tar").arg("-czf").arg(&archive).arg("-C").arg(dir.join("src")).arg(top).status();
        assert!(status.unwrap().success());
        archive
    }

    #[test]
    fn a_new_executable_replaces_the_installed_one() {
        let dir = std::env::temp_dir().join(format!("wercode-stage-exe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/wercode")).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("src/wercode/wercode"), "new").unwrap();
        let installed = dir.join("bin/wercode");
        std::fs::write(&installed, "old").unwrap();

        let staged = stage_executable(&pack(&dir, "wercode"), &installed).unwrap();
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), "old", "nothing changes before quitting");
        apply(&staged, false).unwrap();
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), "new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_bundle_replaces_the_installed_one() {
        let dir = std::env::temp_dir().join(format!("wercode-stage-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/Wercode.app/Contents")).unwrap();
        std::fs::write(dir.join("src/Wercode.app/Contents/version"), "new").unwrap();
        let installed = dir.join("Applications/Wercode.app");
        std::fs::create_dir_all(installed.join("Contents")).unwrap();
        std::fs::write(installed.join("Contents/version"), "old").unwrap();

        let staged = stage_bundle(&pack(&dir, "Wercode.app"), &installed).unwrap();
        apply(&staged, false).unwrap();
        assert_eq!(std::fs::read_to_string(installed.join("Contents/version")).unwrap(), "new");
        assert!(!dir.join("Applications/.Wercode-old.app").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_bundle_is_found_from_its_executable() {
        let exe = Path::new("/Applications/Wercode.app/Contents/MacOS/wercode");
        assert_eq!(installed_bundle(exe), Some(PathBuf::from("/Applications/Wercode.app")));
        assert_eq!(installed_bundle(Path::new("/usr/local/bin/wercode")), None);
    }
}
