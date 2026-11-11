//! Icons compiled into the binary: the component library's default set plus
//! the few extra ones this app draws.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, Result, SharedString};

// An icon used through `gpui_kit::assets::IconName` must be listed here,
// otherwise it silently draws as nothing.
icon_assets!(ExtraIcons, [ChevronsDownUp, FilePlus, FolderPlus, GitBranch]);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(data) => Ok(Some(data)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = Assets.list(path)?;
        all.extend(ExtraIcons.list(path)?);
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AssetSource as _;
    use gpui_kit::assets::IconName;

    use super::AppAssets;

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
