//! Finding a shell to run.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Shell {
    pub program: PathBuf,
    pub args: Vec<String>,
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Resolves the `shell` setting. "auto" picks the best shell on this machine.
pub fn resolve(setting: &str, extra_args: &[String]) -> Shell {
    if setting != "auto" && !setting.is_empty() {
        let p = Path::new(setting);
        let program = if p.is_file() {
            p.to_path_buf()
        } else {
            find_in_path(setting).unwrap_or_else(|| p.to_path_buf())
        };
        return Shell { program, args: extra_args.to_vec() };
    }
    let mut shell = auto();
    shell.args.extend(extra_args.iter().cloned());
    shell
}

#[cfg(windows)]
fn auto() -> Shell {
    for name in ["pwsh.exe", "powershell.exe"] {
        if let Some(program) = find_in_path(name) {
            return Shell { program, args: vec!["-NoLogo".into()] };
        }
    }
    let cmd = std::env::var_os("ComSpec")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"));
    Shell { program: cmd, args: Vec::new() }
}

#[cfg(not(windows))]
fn auto() -> Shell {
    let program = std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    Shell { program, args: vec!["-l".into()] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_finds_something() {
        let s = resolve("auto", &[]);
        assert!(!s.program.as_os_str().is_empty());
    }
}
