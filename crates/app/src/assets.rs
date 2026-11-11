//! Icons compiled into the binary: the component library's default set plus
//! the few extra ones this app draws.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

// An icon used through `gpui_kit::assets::IconName` must be listed here,
// otherwise it silently draws as nothing.
icon_assets!(ExtraIcons, [ChevronsDownUp, FilePlus, FolderPlus, GitBranch]);

/// Embeds the named pictures from one folder of `assets/`.
macro_rules! embed {
    ($folder:literal, [$($name:literal),* $(,)?]) => {
        &[$((
            concat!($folder, "/", $name, ".svg"),
            include_bytes!(concat!("../../../assets/", $folder, "/", $name, ".svg")) as &[u8],
        )),*]
    };
}

/// Coloured file-type icons (Material Icon Theme, MIT) and the app's own mark.
/// Unlike the icons above these keep their colours, so they are drawn with `img`.
#[rustfmt::skip]
const PICTURES: &[(&str, &[u8])] = embed!("icons/files", [
    "audio", "c", "changelog", "cmake", "console", "cpp", "csharp", "css", "database", "docker", "document", "folder", "folder-config", "folder-dist", "folder-docs", "folder-git", "folder-github", "folder-images", "folder-node", "folder-scripts", "folder-src", "folder-target", "folder-test", "folder-tools", "font", "git", "go", "html", "image", "java", "javascript", "json", "key", "kotlin", "license", "lock", "lua", "makefile", "markdown", "nodejs", "pdf", "php", "powershell", "python", "react", "react_ts", "readme", "ruby", "rust", "sass", "settings", "svelte", "svg", "swift", "table", "toml", "tune", "typescript", "video", "vue", "xml", "yaml", "zip",
]);
const BRAND: &[(&str, &[u8])] = embed!("brand", ["logo"]);

pub const LOGO: &str = "brand/logo.svg";

/// Asset path of a file-type icon, if that icon is bundled.
pub fn file_icon(name: &str) -> Option<&'static str> {
    PICTURES.iter().map(|(path, _)| *path).find(|path| {
        path.strip_prefix("icons/files/").and_then(|p| p.strip_suffix(".svg")) == Some(name)
    })
}

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, data)) = PICTURES.iter().chain(BRAND).find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(*data)));
        }
        match ExtraIcons.load(path)? {
            Some(data) => Ok(Some(data)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = Assets.list(path)?;
        all.extend(ExtraIcons.list(path)?);
        all.extend(PICTURES.iter().chain(BRAND).filter(|(p, _)| p.starts_with(path)).map(|(p, _)| (*p).into()));
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;
    use gpui_kit::assets::IconName;

    use super::AppAssets;

    #[test]
    fn every_file_type_has_its_picture() {
        use wercode_core::file_icons::{for_file, for_folder};
        let files = ["Cargo.toml", "a.rs", "a.md", "a.json", "a.py", "a.ts", "a.tsx", "a.png", "a.lock", "a.ps1",
            "a.sh", "a.yml", "a.toml", ".gitignore", "README.md", "LICENSE", "a.txt", "a.zip", "a.sql", "a.csv"];
        for file in files {
            let icon = for_file(file).unwrap_or_else(|| panic!("{file} has no icon"));
            assert!(super::file_icon(icon).is_some(), "{icon}.svg is not bundled");
        }
        for folder in ["src", ".github", "docs", "tests", "assets", "target", "tools", "scripts", "x", "dist",
            "node_modules", "config", ".git"] {
            assert!(super::file_icon(for_folder(folder)).is_some(), "{folder}");
        }
        assert!(matches!(AppAssets.load(super::LOGO), Ok(Some(_))));
    }

    #[test]
    fn every_icon_the_app_draws_is_bundled() {
        let used = [
            IconName::GitBranch,
            IconName::FilePlus,
            IconName::FolderPlus,
            IconName::ChevronsDownUp,
            IconName::RefreshCw,
            IconName::ArrowUp,
            IconName::ArrowDown,
            IconName::TriangleAlert,
            IconName::Folder,
            IconName::Search,
        ];
        for icon in used {
            let path = icon.path();
            assert!(matches!(AppAssets.load(&path), Ok(Some(_))), "{path} is not bundled");
        }
    }
}
