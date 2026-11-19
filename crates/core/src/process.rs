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

/// Who reads the keys typed into a terminal on Windows, as far as a new line
/// (Shift+Enter) goes. The programs disagree on what a new line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyReader {
    /// A Windows console program (PowerShell, Codex, cmd): takes whole key
    /// presses, Shift+Enter included.
    Console,
    /// Claude Code on Windows: reads Enter without Shift, and takes a
    /// Ctrl+Enter press for a new line.
    Claude,
    /// Linux programs behind `wsl.exe`, which read bytes as on Linux.
    Linux,
}

/// What reads the keys of the shell `pid` and the programs it started.
#[cfg(windows)]
pub fn key_reader(pid: u32) -> KeyReader {
    let programs = descendants(&snapshot(), pid);
    reader_of(&programs, |pid| unsafe { command_line(pid) })
}

/// Outside Windows every program reads bytes; the question does not come up.
#[cfg(not(windows))]
pub fn key_reader(_pid: u32) -> KeyReader {
    KeyReader::Console
}

/// Claude Code anywhere under the shell decides; it runs as `claude.exe`, or
/// as `node` or `bun` with its script on the command line when installed
/// with npm. Otherwise WSL decides, then the console.
fn reader_of(programs: &[(u32, String)], command_line: impl Fn(u32) -> Option<String>) -> KeyReader {
    let claude = programs.iter().any(|(pid, name)| match name.as_str() {
        "claude" => true,
        "node" | "bun" => command_line(*pid).is_some_and(|line| line.to_lowercase().contains("claude")),
        _ => false,
    });
    if claude {
        KeyReader::Claude
    } else if programs.iter().any(|(_, name)| name == "wsl" || name == "wslhost") {
        KeyReader::Linux
    } else {
        KeyReader::Console
    }
}

/// Every running process as (process, parent, program file name).
#[cfg(windows)]
fn snapshot() -> Vec<(u32, u32, String)> {
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
            return table;
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
    table
}

/// The processes under `root` (children, their children and so on), each
/// with its program name in lower case and without `.exe`.
fn descendants(table: &[(u32, u32, String)], root: u32) -> Vec<(u32, String)> {
    let mut seen = vec![root];
    let mut parents = vec![root];
    let mut found = Vec::new();
    while let Some(parent) = parents.pop() {
        for (pid, ppid, program) in table {
            // Windows reuses numbers, so a process can look like the parent
            // of an older one; each is taken once.
            if *ppid == parent && !seen.contains(pid) {
                seen.push(*pid);
                parents.push(*pid);
                let file = program.rsplit(['/', '\\']).next().unwrap_or(program).to_lowercase();
                found.push((*pid, file.strip_suffix(".exe").unwrap_or(&file).to_string()));
            }
        }
    }
    found
}

/// The command line a process was started with, if the system tells.
///
/// # Safety
/// Plain calls on a process handle that is opened and closed here.
#[cfg(windows)]
unsafe fn command_line(pid: u32) -> Option<String> {
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, UNICODE_STRING};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut needed = 0u32;
        NtQueryInformationProcess(process, ProcessCommandLineInformation, std::ptr::null_mut(), 0, &mut needed);
        // u64 cells keep the UNICODE_STRING at its start aligned.
        let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(4)];
        let status = NtQueryInformationProcess(
            process,
            ProcessCommandLineInformation,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        );
        CloseHandle(process);
        if status < 0 {
            return None;
        }
        let text = &*(buffer.as_ptr() as *const UNICODE_STRING);
        if text.Buffer.is_null() {
            return None;
        }
        let units = std::slice::from_raw_parts(text.Buffer, text.Length as usize / 2);
        Some(String::from_utf16_lossy(units))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_up_pid_has_no_children() {
        assert!(!has_children(u32::MAX - 7));
        assert_eq!(key_reader(u32::MAX - 7), KeyReader::Console);
    }

    #[test]
    fn programs_under_a_shell_are_found_at_any_depth() {
        let table = [
            (10, 1, "pwsh.exe".to_string()),
            (11, 10, "node.exe".to_string()),
            (12, 11, "Codex.exe".to_string()),
            (13, 1, "claude.exe".to_string()),
            (14, 10, "/usr/local/bin/claude".to_string()),
        ];
        let mut names: Vec<String> = descendants(&table, 10).into_iter().map(|(_, name)| name).collect();
        names.sort();
        assert_eq!(names, ["claude", "codex", "node"]);
        assert!(descendants(&table, 12).is_empty());
    }

    #[test]
    fn claude_code_is_told_apart_from_other_programs() {
        let program = |name: &str| (7, name.to_string());
        let no_line = |_| None;
        assert_eq!(reader_of(&[program("claude")], no_line), KeyReader::Claude);
        assert_eq!(reader_of(&[program("codex")], no_line), KeyReader::Console);
        assert_eq!(reader_of(&[], no_line), KeyReader::Console);
        assert_eq!(reader_of(&[program("wsl")], no_line), KeyReader::Linux);
        // Installed with npm, Claude Code is node running its script.
        let npm = |_| Some(r#""C:\Program Files\nodejs\node.exe" C:\Users\me\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js"#.to_string());
        assert_eq!(reader_of(&[program("node")], npm), KeyReader::Claude);
        let other = |_| Some(r"node C:\Users\me\project\server.js".to_string());
        assert_eq!(reader_of(&[program("node")], other), KeyReader::Console);
    }

    #[cfg(windows)]
    #[test]
    fn the_command_line_of_a_child_is_read() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping", "-n", "3", "127.0.0.1", "wercode-marker"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let line = unsafe { command_line(child.id()) };
        let programs = descendants(&snapshot(), std::process::id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(line.unwrap_or_default().contains("wercode-marker"));
        assert!(programs.iter().any(|(_, name)| name == "cmd"), "{programs:?}");
    }
}
