//! Which picture a file or folder gets in the tree. Returns the icon's name;
//! the app owns the pictures themselves.

/// Icon for a file, by its exact name first and its extension second.
/// `None` means the plain file icon.
pub fn for_file(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "cargo.toml" | "cargo.lock" | "rust-toolchain.toml" | "rustfmt.toml" => "rust",
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitkeep" => "git",
        "dockerfile" | "docker-compose.yml" | "docker-compose.yaml" | ".dockerignore" => "docker",
        "package.json" | "package-lock.json" | ".npmrc" | ".nvmrc" => "nodejs",
        "makefile" | "justfile" => "makefile",
        "cmakelists.txt" => "cmake",
        "license" | "license.md" | "license.txt" | "license-mit" | "license-apache" | "copying" => "license",
        "readme" | "readme.md" | "readme.txt" => "readme",
        "changelog" | "changelog.md" => "changelog",
        ".env" | ".editorconfig" => "tune",
        _ => "",
    };
    if !by_name.is_empty() {
        return Some(by_name);
    }
    if lower.starts_with(".env.") {
        return Some("tune");
    }
    let (_, extension) = lower.rsplit_once('.')?;
    Some(match extension {
        "rs" => "rust",
        "toml" => "toml",
        "md" | "markdown" | "mdx" => "markdown",
        "json" | "jsonc" | "json5" => "json",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "jsx" => "react",
        "tsx" => "react_ts",
        "py" | "pyi" | "pyw" => "python",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" | "less" => "sass",
        "yml" | "yaml" => "yaml",
        "ps1" | "psm1" | "psd1" => "powershell",
        "sh" | "bash" | "zsh" | "fish" | "bat" | "cmd" => "console",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" => "cpp",
        "cs" => "csharp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "php" => "php",
        "rb" => "ruby",
        "lua" => "lua",
        "vue" => "vue",
        "svelte" => "svelte",
        "svg" => "svg",
        "xml" | "xsd" | "xsl" | "plist" => "xml",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "avif" => "image",
        "mp4" | "mov" | "mkv" | "webm" | "avi" => "video",
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => "audio",
        "ttf" | "otf" | "woff" | "woff2" => "font",
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "xz" | "bz2" => "zip",
        "pdf" => "pdf",
        "sql" | "db" | "sqlite" | "sqlite3" => "database",
        "csv" | "tsv" | "xlsx" | "xls" => "table",
        "lock" => "lock",
        "pem" | "key" | "crt" | "cer" | "pub" => "key",
        "ini" | "cfg" | "conf" | "config" => "settings",
        "txt" | "log" | "rtf" => "document",
        _ => return None,
    })
}

/// Icon for a folder: a few well-known names get their own, the rest the plain one.
pub fn for_folder(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "src" | "source" | "crates" | "lib" | "app" => "folder-src",
        ".git" => "folder-git",
        ".github" => "folder-github",
        "docs" | "doc" | "documentation" => "folder-docs",
        "test" | "tests" | "spec" | "__tests__" => "folder-test",
        "assets" | "images" | "img" | "icons" | "static" | "public" => "folder-images",
        "target" | "build" | "out" => "folder-target",
        "dist" => "folder-dist",
        "node_modules" => "folder-node",
        "config" | "configs" | ".config" | ".vscode" => "folder-config",
        "tools" | "tool" => "folder-tools",
        "scripts" | "script" | "bin" => "folder-scripts",
        _ => "folder",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_win_over_extensions_and_case_is_ignored() {
        assert_eq!(for_file("Cargo.toml"), Some("rust"));
        assert_eq!(for_file("settings.TOML"), Some("toml"));
        assert_eq!(for_file("main.rs"), Some("rust"));
        assert_eq!(for_file(".gitignore"), Some("git"));
        assert_eq!(for_file("README.md"), Some("readme"));
        assert_eq!(for_file("notes.md"), Some("markdown"));
        assert_eq!(for_file(".env.local"), Some("tune"));
        assert_eq!(for_file("archive.tar.gz"), Some("zip"));
        assert_eq!(for_file("no_extension"), None);
        assert_eq!(for_file("weird.qqq"), None);
    }

    #[test]
    fn folders() {
        assert_eq!(for_folder("src"), "folder-src");
        assert_eq!(for_folder(".github"), "folder-github");
        assert_eq!(for_folder("whatever"), "folder");
    }
}
