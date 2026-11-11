//! Asking the OS whether a process has children.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_up_pid_has_no_children() {
        assert!(!has_children(u32::MAX - 7));
    }
}
