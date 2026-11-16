//! Tasks: the commands a project already describes, found in its own files
//! (`package.json` scripts, Cargo and Go commands, `Makefile` targets,
//! `justfile` recipes), so they can be run from a list.

use std::path::Path;

/// One command a project offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// The command line, as it is typed into a shell in the project folder.
    pub command: String,
    /// The file that describes it: `package.json`, `Makefile`.
    pub source: &'static str,
}

/// The program that runs the scripts of `package.json`: the one whose lock
/// file is in the folder, or npm.
fn package_runner(root: &Path) -> &'static str {
    let has = |name: &str| root.join(name).is_file();
    if has("pnpm-lock.yaml") {
        "pnpm"
    } else if has("yarn.lock") {
        "yarn"
    } else if has("bun.lockb") || has("bun.lock") {
        "bun"
    } else {
        "npm"
    }
}

/// The names of the scripts in the text of a `package.json`, by name.
pub fn package_scripts(text: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(package)) = serde_json::from_str(text) else { return Vec::new() };
    let mut names: Vec<String> = match package.get("scripts") {
        Some(serde_json::Value::Object(scripts)) => scripts.keys().cloned().collect(),
        _ => Vec::new(),
    };
    // The reader keeps the order of the file in some builds and sorts in
    // others; sorted, the list is the same everywhere.
    names.sort();
    names
}

/// Whether a name can be given to `make` or `just` as it stands.
fn is_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
}

/// The targets of a `Makefile` that are meant to be asked for: not the
/// special ones (`.PHONY`), not patterns (`%.o`), not variables (`CC := gcc`).
pub fn make_targets(text: &str) -> Vec<String> {
    let mut targets: Vec<String> = Vec::new();
    for line in text.lines() {
        // Recipes are indented; everything about a target starts its line.
        if line.starts_with(['\t', ' ', '#']) {
            continue;
        }
        let Some((names, rest)) = line.split_once(':') else { continue };
        if rest.starts_with('=') || names.contains(['=', '$', '%']) {
            continue;
        }
        for name in names.split_whitespace() {
            if is_name(name) && !name.starts_with('.') && !targets.iter().any(|t| t == name) {
                targets.push(name.to_string());
            }
        }
    }
    targets
}

/// The recipes of a `justfile`. Private ones (`_name`) are left out.
pub fn just_recipes(text: &str) -> Vec<String> {
    let mut recipes: Vec<String> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t', '#', '[']) {
            continue;
        }
        let Some((head, rest)) = line.split_once(':') else { continue };
        // `name := value` sets a variable.
        if rest.starts_with('=') {
            continue;
        }
        // A recipe may be marked quiet with `@` and may take parameters.
        let Some(name) = head.split_whitespace().next().map(|name| name.trim_start_matches('@')) else { continue };
        let keyword = matches!(name, "set" | "alias" | "export" | "import" | "mod");
        if is_name(name) && !keyword && !name.starts_with('_') && !recipes.iter().any(|r| r == name) {
            recipes.push(name.to_string());
        }
    }
    recipes
}

/// Everything the project in `root` says it can run. Reads a few small
/// files; call it off the UI thread.
pub fn discover(root: &Path) -> Vec<Task> {
    let read = |name: &str| std::fs::read_to_string(root.join(name)).ok();
    let mut tasks = Vec::new();
    let mut add = |source: &'static str, command: String| tasks.push(Task { command, source });

    if let Some(text) = read("package.json") {
        let runner = package_runner(root);
        for script in package_scripts(&text) {
            add("package.json", format!("{runner} run {script}"));
        }
    }
    if root.join("Cargo.toml").is_file() {
        for command in ["cargo run", "cargo test", "cargo build", "cargo check"] {
            add("Cargo.toml", command.to_string());
        }
    }
    if root.join("go.mod").is_file() {
        for command in ["go run .", "go test ./...", "go build ./..."] {
            add("go.mod", command.to_string());
        }
    }
    for name in ["Makefile", "makefile", "GNUmakefile"] {
        if let Some(text) = read(name) {
            for target in make_targets(&text) {
                add("Makefile", format!("make {target}"));
            }
            break;
        }
    }
    for name in ["justfile", "Justfile", ".justfile"] {
        if let Some(text) = read(name) {
            for recipe in just_recipes(&text) {
                add("justfile", format!("just {recipe}"));
            }
            break;
        }
    }
    tasks
}

/// The tasks that have every word of `query` in them, as indexes into
/// `tasks`, in their order. Upper and lower case count as the same.
pub fn matches(tasks: &[Task], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            let command = task.command.to_lowercase();
            words.iter().all(|word| command.contains(word))
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_targets_and_recipes_are_read() {
        let package = r#"{"name": "site", "scripts": {"dev": "vite", "build": "vite build", "test:unit": "vitest"}}"#;
        assert_eq!(package_scripts(package), ["build", "dev", "test:unit"]);
        assert!(package_scripts("{ not json").is_empty());
        assert!(package_scripts(r#"{"scripts": []}"#).is_empty());

        let makefile = "CC := gcc\nOBJ = a.o\n.PHONY: all clean\nall: build test\n\tcc -o app\nbuild test: $(OBJ)\n%.o: %.c\nclean:\n\trm -f app\n# docs: no\n";
        assert_eq!(make_targets(makefile), ["all", "build", "test", "clean"]);

        let justfile = "set shell := [\"bash\", \"-c\"]\nversion := \"1.0\"\n\n# Build it\nbuild:\n    cargo build\n\n@test filter='':\n    cargo test {{filter}}\n\n[private]\n_helper:\n    true\nalias b := build\n";
        assert_eq!(just_recipes(justfile), ["build", "test"]);
    }

    #[test]
    fn a_project_lists_what_it_can_run() {
        let dir = std::env::temp_dir().join(format!("wercode-tasks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(discover(&dir).is_empty());
        std::fs::write(dir.join("package.json"), r#"{"scripts": {"dev": "vite"}}"#).unwrap();
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.join("Makefile"), "lint:\n\ttrue\n").unwrap();
        let tasks = discover(&dir);
        let commands: Vec<&str> = tasks.iter().map(|t| t.command.as_str()).collect();
        assert_eq!(commands, ["pnpm run dev", "cargo run", "cargo test", "cargo build", "cargo check", "make lint"]);
        assert_eq!((tasks[0].source, tasks[5].source), ("package.json", "Makefile"));

        assert_eq!(matches(&tasks, ""), [0, 1, 2, 3, 4, 5]);
        assert_eq!(matches(&tasks, "CARGO t"), [2]);
        assert_eq!(matches(&tasks, "run"), [0, 1]);
        assert!(matches(&tasks, "deploy").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
