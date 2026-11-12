//! Directory listing for the file tree. One level at a time, .gitignore aware.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Lists the direct children of `dir`: folders first, then files, by name.
/// Ignored files and the `.git` folder are left out.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let walker = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .require_git(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    let mut out: Vec<Entry> = walker
        .filter_map(Result::ok)
        .filter(|e| e.depth() == 1)
        .map(|e| Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            is_dir: e.file_type().is_some_and(|t| t.is_dir()),
            path: e.into_path(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// An entry of the tree as far as it has been read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub entry: Entry,
    /// What is inside an open folder. A closed folder is not read at all.
    pub children: Vec<Node>,
    /// A closed folder that has something in it, so it can be opened.
    pub has_more: bool,
}

/// Reads `dir` and, below it, only the folders listed in `open`. `budget` is
/// how many entries may still be read; it stops a huge folder from taking forever.
pub fn read_tree(dir: &Path, open: &HashSet<PathBuf>, budget: &mut usize) -> Vec<Node> {
    let mut nodes = Vec::new();
    for entry in list_dir(dir) {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let (mut children, mut has_more) = (Vec::new(), false);
        if entry.is_dir {
            if open.contains(&entry.path) {
                children = read_tree(&entry.path, open, budget);
            } else {
                has_more = std::fs::read_dir(&entry.path).is_ok_and(|mut inside| inside.next().is_some());
            }
        }
        nodes.push(Node { entry, children, has_more });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_dirs_first_and_skips_ignored() {
        let root = std::env::temp_dir().join(format!("wercode-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(root.join("A.txt"), "").unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

        let names: Vec<String> = list_dir(&root).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["src", ".gitignore", "A.txt", "b.txt"]);

        // Only open folders are read; a closed one just says whether it has anything.
        std::fs::create_dir_all(root.join("src").join("deep")).unwrap();
        std::fs::write(root.join("src").join("deep").join("x.rs"), "").unwrap();
        let mut budget = 100;
        let closed = read_tree(&root, &HashSet::new(), &mut budget);
        assert_eq!(closed.len(), 4);
        assert!(closed[0].has_more && closed[0].children.is_empty());
        assert_eq!(budget, 96);
        let open = HashSet::from([root.join("src")]);
        let tree = read_tree(&root, &open, &mut budget);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].entry.name, "deep");
        assert!(tree[0].children[0].has_more && tree[0].children[0].children.is_empty());
        // The budget cuts the listing short.
        assert_eq!(read_tree(&root, &open, &mut 2).len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
