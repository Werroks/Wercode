//! Asking the OS about the processes a shell has started.

/// True when at least one running process has `pid` as its parent.
#[cfg(windows)]
pub fn has_children(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: plain Win32 calls on a snapshot handle we own; the entry is a
    // zeroed plain-data struct with its size field set as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32ParentProcessID == pid && entry.th32ProcessID != pid {
                    found = true;
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

/// True when at least one running process has `pid` as its parent.
#[cfg(not(windows))]
pub fn has_children(pid: u32) -> bool {
    std::process::Command::new("pgrep")
        .arg("-P")
        .arg(pid.to_string())
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

/// The names of the programs running under `pid`: its children, their
/// children and so on, in lower case and without `.exe`.
#[cfg(windows)]
pub fn descendant_names(pid: u32) -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut table = Vec::new();
    // SAFETY: as in `has_children`.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = &entry.szExeFile;
                let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                table.push((entry.th32ProcessID, entry.th32ParentProcessID, String::from_utf16_lossy(&name[..end])));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    descendants(&table, pid)
}

/// The names of the programs running under `pid`: its children, their
/// children and so on, in lower case.
#[cfg(not(windows))]
pub fn descendant_names(pid: u32) -> Vec<String> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=,ppid=,comm="]).output() else {
        return Vec::new();
    };
    let table: Vec<(u32, u32, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let parent = parts.next()?.parse().ok()?;
            let command = parts.collect::<Vec<_>>().join(" ");
            Some((pid, parent, command))
        })
        .collect();
    descendants(&table, pid)
}

/// The names under `root` in a table of (process, parent, program). A
/// program may be given as a path (macOS does that); only its file name counts.
fn descendants(table: &[(u32, u32, String)], root: u32) -> Vec<String> {
    let mut seen = vec![root];
    let mut parents = vec![root];
    let mut names = Vec::new();
    while let Some(parent) = parents.pop() {
        for (pid, ppid, program) in table {
            // Windows reuses numbers, so a parent may list a newer process
            // as its child twice over; each is taken once.
            if *ppid == parent && !seen.contains(pid) {
                seen.push(*pid);
                parents.push(*pid);
                let file = program.rsplit(['/', '\\']).next().unwrap_or(program).to_lowercase();
                names.push(file.strip_suffix(".exe").unwrap_or(&file).to_string());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_up_pid_has_no_children() {
        assert!(!has_children(u32::MAX - 7));
        assert!(descendant_names(u32::MAX - 7).is_empty());
    }

    #[test]
    fn programs_under_a_shell_are_found_at_any_depth() {
        let table = [
            (10, 1, "pwsh.exe".to_string()),
            (11, 10, "node.exe".to_string()),
            (12, 11, "Codex.exe".to_string()),
            (13, 1, "claude.exe".to_string()),
            (14, 10, "/usr/local/bin/claude".to_string()),
            (0, 0, "[System Process]".to_string()),
        ];
        let mut names = descendants(&table, 10);
        names.sort();
        assert_eq!(names, ["claude", "codex", "node"]);
        assert!(descendants(&table, 12).is_empty());
    }

    #[test]
    fn a_child_of_this_process_is_found() {
        let me = std::process::id();
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "sleep" })
            .args(if cfg!(windows) { &["/c", "ping", "-n", "3", "127.0.0.1"][..] } else { &["2"][..] })
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let names = descendant_names(me);
        let _ = child.kill();
        let _ = child.wait();
        let wanted = if cfg!(windows) { "cmd" } else { "sleep" };
        assert!(names.iter().any(|n| n == wanted), "{names:?}");
    }
}
