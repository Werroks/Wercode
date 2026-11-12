//! What the file tree does to files: rename, move to the trash, and write a
//! path the way a shell wants it.

use std::path::{Path, PathBuf};

/// Gives a file or folder a new name in the same folder. Refuses a name that
/// is taken, so nothing is ever overwritten.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let name = new_name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err("That is not a name a file can have.".into());
    }
    let parent = path.parent().ok_or("This cannot be renamed.")?;
    let target = parent.join(name);
    if target == path {
        return Ok(target);
    }
    // Changing only the case of a name is fine where names ignore case.
    let same_file = path.to_string_lossy().to_lowercase() == target.to_string_lossy().to_lowercase();
    if target.exists() && !same_file {
        return Err(format!("{name} already exists."));
    }
    std::fs::rename(path, &target).map_err(|e| format!("Could not rename: {e}"))?;
    Ok(target)
}

/// Moves a file or folder to the system trash. Nothing is deleted for good.
pub fn trash(path: &Path) -> Result<(), String> {
    trash::delete(path).map_err(|e| format!("Could not move it to the trash: {e}"))
}

/// The path as text to type into a shell: relative to the shell's folder when
/// it is inside it, with forward slashes left alone, and in double quotes if
/// it has characters a shell would split or expand.
pub fn for_shell(path: &Path, cwd: Option<&Path>) -> String {
    let shown = match cwd.and_then(|cwd| path.strip_prefix(cwd).ok()) {
        Some(inside) if !inside.as_os_str().is_empty() => inside,
        Some(_) => Path::new("."),
        None => path,
    };
    let text = shown.to_string_lossy();
    let plain = text.chars().all(|c| c.is_alphanumeric() || "/\\:._-~+".contains(c));
    if plain { text.into_owned() } else { format!("\"{}\"", text.replace('"', "\\\"")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("wercode-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("folder")).unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();

        assert_eq!(rename(&dir.join("a.txt"), " c.txt ").unwrap(), dir.join("c.txt"));
        assert_eq!(std::fs::read_to_string(dir.join("c.txt")).unwrap(), "a");
        assert!(!dir.join("a.txt").exists());
        // A taken name, an empty one and one with a folder in it are refused.
        assert!(rename(&dir.join("c.txt"), "b.txt").is_err());
        assert!(rename(&dir.join("c.txt"), "  ").is_err());
        assert!(rename(&dir.join("c.txt"), "folder/c.txt").is_err());
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "b");
        // The same name is not an error, and a folder renames like a file.
        assert_eq!(rename(&dir.join("c.txt"), "c.txt").unwrap(), dir.join("c.txt"));
        assert_eq!(rename(&dir.join("folder"), "other").unwrap(), dir.join("other"));
        assert!(dir.join("other").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writes_paths_for_a_shell() {
        let root = Path::new("/work/app");
        assert_eq!(for_shell(&root.join("src").join("main.rs"), Some(root)).replace('\\', "/"), "src/main.rs");
        assert_eq!(for_shell(root, Some(root)), ".");
        assert_eq!(for_shell(Path::new("/other/a.rs"), Some(root)), "/other/a.rs");
        assert_eq!(for_shell(Path::new("/other/a.rs"), None), "/other/a.rs");
        assert_eq!(for_shell(&root.join("my file.txt"), Some(root)), "\"my file.txt\"");
        assert_eq!(for_shell(&root.join("a&b.txt"), Some(root)), "\"a&b.txt\"");
        assert_eq!(for_shell(&root.join("\u{444}\u{430}\u{439}\u{43b}.txt"), Some(root)), "\u{444}\u{430}\u{439}\u{43b}.txt");
    }
}
