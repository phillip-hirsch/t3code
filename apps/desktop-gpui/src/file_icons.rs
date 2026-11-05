//! Per-file-type icons (`PierreEntryIcon.tsx`, `pierre-icons.ts`): the web's Pierre tree icons
//! with T3's overrides. The table is generated from the web's sprite and rules.

#[path = "file_icons_table.rs"]
mod table;

use gpui_kit::{Hsla, rgb};

pub use table::SVGS;
use table::{DEFAULT, EXTENSIONS, FILE_NAMES, IconRule, T3_EXTENSIONS, T3_FILE_NAMES};

/// `getExtensionCandidates`: `a.test.ts` gives `test.ts`, then `ts`.
fn extension_candidates(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('.').map(move |(at, _)| &name[at + 1..])
}

fn find(rules: &'static [IconRule], key: &str) -> Option<&'static IconRule> {
    rules.iter().find(|rule| rule.0 == key)
}

/// `createFileTreeIconResolver(T3_PIERRE_ICONS).resolveIcon`: T3's file names, then its
/// extensions, then the built-in names and extensions, else the default glyph.
fn rule(path: &str) -> &'static IconRule {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_lowercase();
    find(T3_FILE_NAMES, &name)
        .or_else(|| extension_candidates(&name).find_map(|ext| find(T3_EXTENSIONS, ext)))
        .or_else(|| find(FILE_NAMES, &name))
        .or_else(|| extension_candidates(&name).find_map(|ext| find(EXTENSIONS, ext)))
        .unwrap_or(&DEFAULT)
}

/// The icon asset for a file and its colour in the current theme.
pub fn file_icon(path: &str, dark: bool) -> (&'static str, Hsla) {
    let (_, asset, light, dark_color) = *rule(path);
    (asset, rgb(if dark { dark_color } else { light }).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_resolve_like_the_web_tree() {
        let asset = |path: &str| rule(path).1;
        assert_eq!(asset("src/main.rs"), "icons/files/file-tree-builtin-rust.svg");
        assert_eq!(asset("web/App.TSX"), "icons/files/file-tree-builtin-react.svg");
        assert_eq!(asset("package.json"), "icons/files/file-tree-builtin-npm.svg", "T3 file names win");
        assert_eq!(asset("data/rows.csv"), "icons/files/t3-file-icon-spreadsheet.svg", "T3 extensions win");
        assert_eq!(asset("clip.MP4"), "icons/files/t3-file-icon-video.svg");
        assert_eq!(asset("LICENSE.unknown"), "icons/files/file-tree-builtin-default.svg");
        assert_eq!(rule("a/b.heic").2, 0xd32a61, "the untokened image icon takes the image colour");
        let names: Vec<&str> = extension_candidates("a.test.ts").collect();
        assert_eq!(names, ["test.ts", "ts"]);
        for (path, _) in SVGS {
            assert!(
                [T3_FILE_NAMES, T3_EXTENSIONS, FILE_NAMES, EXTENSIONS].concat().iter().any(|r| r.1 == *path)
                    || *path == DEFAULT.1,
                "{path} is used"
            );
        }
    }
}
