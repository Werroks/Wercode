//! Which language servers Wercode knows, which files each one checks, and
//! how to find them on this computer.

use std::path::{Path, PathBuf};

/// One way to run a server.
#[derive(Debug, Clone)]
pub struct Program {
    /// A program name looked up on `PATH`, or a full path.
    pub program: String,
    pub args: Vec<String>,
    /// Added to the server's environment.
    pub env: Vec<(String, String)>,
}

/// A language server Wercode can start.
#[derive(Debug, Clone)]
pub struct Spec {
    /// What the status bar calls it until it is running.
    pub name: String,
    /// Ways to run it, best first; the first one found on this computer is used.
    pub programs: Vec<Program>,
    /// Arguments that make the program print its version and exit. A
    /// program that fails them counts as missing; rustup puts a stand-in
    /// for `rust-analyzer` on `PATH` that only says it is not installed.
    pub probe: Option<Vec<String>>,
    /// The language ids of the protocol, such as `rust` or `typescriptreact`.
    pub languages: Vec<&'static str>,
    /// Files that mark the top folder of a project in that language.
    pub markers: Vec<&'static str>,
    /// A command that installs the server, run in a terminal when asked.
    pub install: Option<String>,
    /// Whether to offer the install when the server is missing. A second
    /// server for the same language (a linter next to a type checker) is
    /// used when it is there, and not asked for when it is not.
    pub offer: bool,
}

fn program(name: &str, args: &[&str]) -> Program {
    Program { program: name.into(), args: args.iter().map(|a| a.to_string()).collect(), env: Vec::new() }
}

fn spec(
    name: &str,
    programs: Vec<Program>,
    languages: &[&'static str],
    markers: &[&'static str],
    install: Option<&str>,
) -> Spec {
    Spec {
        name: name.into(),
        programs,
        probe: None,
        languages: languages.to_vec(),
        markers: markers.to_vec(),
        install: install.map(Into::into),
        offer: true,
    }
}

fn clangd_install() -> Option<&'static str> {
    if cfg!(windows) {
        Some("winget install LLVM.LLVM")
    } else if cfg!(target_os = "macos") {
        Some("brew install llvm")
    } else {
        Some("sudo apt install clangd")
    }
}

/// The servers Wercode looks for.
pub fn builtin() -> Vec<Spec> {
    const JS: &[&str] = &["typescript", "typescriptreact", "javascript", "javascriptreact"];
    const NODE_MARKERS: &[&str] = &["tsconfig.json", "jsconfig.json", "package.json"];
    const PYTHON_MARKERS: &[&str] =
        &["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "pyrightconfig.json"];
    const VSCODE: &str = "npm install -g vscode-langservers-extracted";
    vec![
        Spec {
            probe: Some(vec!["--version".into()]),
            ..spec(
                "rust-analyzer",
                vec![program("rust-analyzer", &[])],
                &["rust"],
                &["Cargo.toml"],
                Some("rustup component add rust-analyzer"),
            )
        },
        spec(
            "typescript-language-server",
            vec![program("typescript-language-server", &["--stdio"])],
            JS,
            NODE_MARKERS,
            Some("npm install -g typescript typescript-language-server"),
        ),
        spec(
            "pyright",
            vec![
                program("basedpyright-langserver", &["--stdio"]),
                program("pyright-langserver", &["--stdio"]),
                program("pylsp", &[]),
            ],
            &["python"],
            PYTHON_MARKERS,
            Some("npm install -g pyright"),
        ),
        Spec {
            offer: false,
            ..spec(
                "ruff",
                vec![program("ruff", &["server"])],
                &["python"],
                PYTHON_MARKERS,
                Some("pip install ruff"),
            )
        },
        spec(
            "gopls",
            vec![program("gopls", &[])],
            &["go"],
            &["go.work", "go.mod"],
            Some("go install golang.org/x/tools/gopls@latest"),
        ),
        spec(
            "clangd",
            vec![program("clangd", &[])],
            &["c", "cpp"],
            &["compile_commands.json", "compile_flags.txt", ".clangd", "CMakeLists.txt"],
            clangd_install(),
        ),
        spec(
            "vscode-json-language-server",
            vec![program("vscode-json-language-server", &["--stdio"])],
            &["json", "jsonc"],
            NODE_MARKERS,
            Some(VSCODE),
        ),
        spec(
            "vscode-css-language-server",
            vec![program("vscode-css-language-server", &["--stdio"])],
            &["css", "scss", "less"],
            NODE_MARKERS,
            Some(VSCODE),
        ),
        spec(
            "vscode-html-language-server",
            vec![program("vscode-html-language-server", &["--stdio"])],
            &["html"],
            NODE_MARKERS,
            Some(VSCODE),
        ),
        spec(
            "yaml-language-server",
            vec![program("yaml-language-server", &["--stdio"])],
            &["yaml"],
            &[],
            Some("npm install -g yaml-language-server"),
        ),
        spec(
            "bash-language-server",
            vec![program("bash-language-server", &["start"])],
            &["shellscript"],
            &[],
            Some("npm install -g bash-language-server"),
        ),
        spec(
            "taplo",
            vec![program("taplo", &["lsp", "stdio"])],
            &["toml"],
            &[],
            Some("cargo install taplo-cli --locked --features lsp"),
        ),
        spec("ruby-lsp", vec![program("ruby-lsp", &[])], &["ruby"], &["Gemfile"], Some("gem install ruby-lsp")),
    ]
}

/// The protocol's language id for a file, from its name.
pub fn language_id(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if matches!(name.as_str(), ".bashrc" | ".bash_profile" | ".profile" | ".zshrc") {
        return Some("shellscript");
    }
    // JSON files that allow comments.
    if matches!(name.as_str(), "tsconfig.json" | "jsconfig.json" | ".eslintrc.json") {
        return Some("jsonc");
    }
    let extension = name.rsplit_once('.')?.1;
    Some(match extension {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "json" => "json",
        "jsonc" => "jsonc",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "html" | "htm" => "html",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" => "shellscript",
        "toml" => "toml",
        "rb" => "ruby",
        _ => return None,
    })
}

/// The name of a language as people write it.
pub fn language_name(id: &str) -> &'static str {
    match id {
        "rust" => "Rust",
        "typescript" | "typescriptreact" => "TypeScript",
        "javascript" | "javascriptreact" => "JavaScript",
        "python" => "Python",
        "go" => "Go",
        "c" => "C",
        "cpp" => "C++",
        "json" | "jsonc" => "JSON",
        "css" => "CSS",
        "scss" => "SCSS",
        "less" => "Less",
        "html" => "HTML",
        "yaml" => "YAML",
        "shellscript" => "Shell",
        "toml" => "TOML",
        "ruby" => "Ruby",
        _ => "",
    }
}

/// Folders where installers put programs that a window started from the
/// desktop may not have on its `PATH` (on macOS such a window gets only the
/// system folders).
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    if let Some(home) = &home {
        dirs.push(home.join(".cargo").join("bin"));
        dirs.push(home.join("go").join("bin"));
        dirs.push(home.join(".local").join("bin"));
    }
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    } else {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
        if let Some(home) = &home {
            dirs.push(home.join(".npm-global").join("bin"));
        }
    }
    dirs
}

/// The folders searched for programs: `PATH`, then [`extra_dirs`].
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for dir in extra_dirs() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The `PATH` a server is started with: what is searched, so a server
/// written in JavaScript finds `node` next to it.
pub fn server_path() -> Option<std::ffi::OsString> {
    std::env::join_paths(search_path()).ok()
}

#[cfg(unix)]
fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(path: &Path) -> bool {
    path.is_file()
}

/// Finds a program in `dirs`. On Windows a name without an extension may be
/// an `.exe`, or a `.cmd` as npm installs them.
pub fn find_program_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let given = Path::new(name);
    if given.is_absolute() {
        return runnable(given).then(|| given.to_path_buf());
    }
    let endings: &[&str] = if cfg!(windows) && given.extension().is_none() {
        &[".exe", ".cmd", ".bat", ".com"]
    } else {
        &[""]
    };
    dirs.iter().flat_map(|dir| endings.iter().map(move |ending| dir.join(format!("{name}{ending}")))).find(|p| runnable(p))
}

pub fn find_program(name: &str) -> Option<PathBuf> {
    find_program_in(name, &search_path())
}

/// The folder a server for `path` should treat as its project: the highest
/// folder inside the open project that holds one of `markers` (a Cargo or
/// npm workspace sits above its members), else the open project itself. A
/// file outside the project gets the nearest folder with a marker, or its own.
pub fn root_for(path: &Path, project: &Path, markers: &[&str]) -> PathBuf {
    let has_marker = |dir: &Path| markers.iter().any(|m| dir.join(m).exists());
    let parent = path.parent().unwrap_or(path);
    if path.starts_with(project) {
        let mut found = None;
        for dir in parent.ancestors() {
            if !dir.starts_with(project) {
                break;
            }
            if has_marker(dir) {
                found = Some(dir.to_path_buf());
            }
        }
        // The project folder itself is the default; a marker above it changes nothing.
        return found.unwrap_or_else(|| project.to_path_buf());
    }
    parent.ancestors().find(|dir| has_marker(dir)).unwrap_or(parent).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_their_language() {
        assert_eq!(language_id(Path::new("src/main.rs")), Some("rust"));
        assert_eq!(language_id(Path::new("App.TSX")), Some("typescriptreact"));
        assert_eq!(language_id(Path::new("tsconfig.json")), Some("jsonc"));
        assert_eq!(language_id(Path::new("data.json")), Some("json"));
        assert_eq!(language_id(Path::new(".zshrc")), Some("shellscript"));
        assert_eq!(language_id(Path::new("README")), None);
        assert_eq!(language_id(Path::new("notes.txt")), None);
        assert_eq!(language_name("cpp"), "C++");
        // Every language a server checks has a name to show.
        for spec in builtin() {
            for language in spec.languages {
                assert!(!language_name(language).is_empty(), "{language}");
            }
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wercode-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_highest_marked_folder_in_the_project_is_the_root() {
        let project = temp("root");
        let member = project.join("crates").join("core");
        std::fs::create_dir_all(member.join("src")).unwrap();
        let file = member.join("src").join("lib.rs");
        // No marker: the project.
        assert_eq!(root_for(&file, &project, &["Cargo.toml"]), project);
        // A member's marker.
        std::fs::write(member.join("Cargo.toml"), "").unwrap();
        assert_eq!(root_for(&file, &project, &["Cargo.toml"]), member);
        // The workspace above it wins.
        std::fs::write(project.join("crates").join("Cargo.toml"), "").unwrap();
        assert_eq!(root_for(&file, &project, &["Cargo.toml"]), project.join("crates"));
        // A file outside the project: the nearest marker.
        let elsewhere = temp("root-outside");
        std::fs::create_dir_all(elsewhere.join("a")).unwrap();
        std::fs::write(elsewhere.join("Cargo.toml"), "").unwrap();
        assert_eq!(root_for(&elsewhere.join("a").join("x.rs"), &project, &["Cargo.toml"]), elsewhere);
        std::fs::remove_dir_all(&project).unwrap();
        std::fs::remove_dir_all(&elsewhere).unwrap();
    }

    #[test]
    fn programs_are_found_by_name() {
        let dir = temp("which");
        let name = if cfg!(windows) { "fake-server.cmd" } else { "fake-server" };
        std::fs::write(dir.join(name), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dirs = vec![dir.join("missing"), dir.clone()];
        assert_eq!(find_program_in("fake-server", &dirs), Some(dir.join(name)));
        assert_eq!(find_program_in("no-such-server", &dirs), None);
        assert_eq!(find_program_in(&dir.join(name).to_string_lossy(), &[]), Some(dir.join(name)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
