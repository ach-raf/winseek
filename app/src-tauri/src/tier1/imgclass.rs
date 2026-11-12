//! Tells a person's pictures from the pictures software ships with.
//!
//! An indexed Documents folder is full of images nobody took: game graphics packs, desktop-skin
//! assets, the icons of a code project. Measured on a real library (2026-11-12): 8,184 of 9,850
//! indexed images were such assets (one football game's graphics folder alone held 6,985), and
//! they drowned the timeline, filled collections with look-alike icons ("medical" matched every
//! green cross) and offered app files for clean-up.
//!
//! Two deterministic rules, no model involved:
//! 1. Small: nothing a camera, a phone or a screen capture produces is under [`SMALL_LONG`] px on
//!    its long side or [`SMALL_SHORT`] px on its short side. Icons, avatars, sprites and banners are.
//! 2. Company: a folder (below an indexed root) whose images are mostly small is an asset folder,
//!    and so is everything else in it: the 1920x1080 backgrounds of a skin pack are not photos.
//!
//! What is left is a screenshot (by name, folder or screen-sized PNG, see `ocr::is_screenshot`) or
//! a photo.

use std::collections::HashMap;

use super::ocr;

/// Long side below this = small.
pub const SMALL_LONG: u32 = 512;
/// Short side below this = small.
pub const SMALL_SHORT: u32 = 200;
/// A folder needs this many images before its mix says anything.
pub const ASSET_DIR_MIN: usize = 12;
/// Share of small images that makes a folder an asset folder (60%).
const ASSET_DIR_NUM: usize = 6;
const ASSET_DIR_DEN: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImgClass {
    Photo,
    Screenshot,
    /// App / game / web asset: hidden from the timeline, collections and clean-up by default.
    Graphic,
}

impl ImgClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ImgClass::Photo => "photo",
            ImgClass::Screenshot => "screenshot",
            ImgClass::Graphic => "graphic",
        }
    }
}

/// `None` and `(0, 0)` (unreadable) are not small: an unknown file keeps the benefit of the doubt.
pub fn is_small(dims: Option<(u32, u32)>) -> bool {
    matches!(dims, Some((w, h)) if w > 0 && h > 0 && (w.max(h) < SMALL_LONG || w.min(h) < SMALL_SHORT))
}

fn norm(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// Byte length of the longest root that contains `path` (both normalised), else of the first two
/// components (`c:\users`), so that a drive or a profile folder is never judged as one folder.
fn floor_len(path: &str, roots: &[String]) -> usize {
    let best = roots
        .iter()
        .filter(|r| path.len() > r.len() && path.starts_with(r.as_str()) && path.as_bytes()[r.len()] == b'\\')
        .map(|r| r.len())
        .max();
    best.unwrap_or_else(|| path.match_indices('\\').nth(1).map_or(path.len(), |(i, _)| i))
}

/// The folders above `path` that may be judged: every ancestor strictly below the floor.
fn ancestors(path: &str, floor: usize) -> impl Iterator<Item = &str> {
    path.match_indices('\\').map(|(i, _)| i).filter(move |i| *i > floor).map(move |i| &path[..i])
}

/// One class per item, in order. `roots` are the indexed folders (the scopes' include list).
pub fn classify(items: &[(&str, Option<(u32, u32)>)], roots: &[String], screenshots_dir: Option<&str>) -> Vec<ImgClass> {
    let roots: Vec<String> = roots.iter().map(|r| norm(r).trim_end_matches('\\').to_string()).collect();
    let paths: Vec<String> = items.iter().map(|(p, _)| norm(p)).collect();
    let floors: Vec<usize> = paths.iter().map(|p| floor_len(p, &roots)).collect();

    // folder -> (images below it, small ones)
    let mut dirs: HashMap<&str, (usize, usize)> = HashMap::new();
    for (i, p) in paths.iter().enumerate() {
        let small = is_small(items[i].1) as usize;
        for a in ancestors(p, floors[i]) {
            let e = dirs.entry(a).or_default();
            e.0 += 1;
            e.1 += small;
        }
    }
    let asset = |d: &str| dirs.get(d).is_some_and(|(n, s)| *n >= ASSET_DIR_MIN && s * ASSET_DIR_DEN >= n * ASSET_DIR_NUM);

    items
        .iter()
        .enumerate()
        .map(|(i, (path, dims))| {
            if is_small(*dims) || ancestors(&paths[i], floors[i]).any(asset) {
                ImgClass::Graphic
            } else if ocr::is_screenshot(path, dims.filter(|d| d.0 > 0), screenshots_dir) {
                ImgClass::Screenshot
            } else {
                ImgClass::Photo
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<String> {
        vec![r"C:\Users\Me\Documents".into(), r"C:\Users\Me\Pictures".into()]
    }

    #[test]
    fn small_is_about_pixels() {
        assert!(is_small(Some((32, 32))));
        assert!(is_small(Some((404, 404))), "an avatar");
        assert!(is_small(Some((1600, 120))), "a banner");
        assert!(!is_small(Some((450, 747))), "a phone capture of a document");
        assert!(!is_small(Some((287, 1600))), "a long chat screenshot");
        assert!(!is_small(Some((0, 0))) && !is_small(None), "unknown is not small");
    }

    #[test]
    fn icons_are_graphics_and_photos_stay() {
        let items = [
            (r"C:\Users\Me\Pictures\cat.jpg", Some((4000, 3000))),
            (r"C:\Users\Me\Pictures\logo.png", Some((64, 64))),
            (r"C:\Users\Me\Pictures\Screenshots\Screenshot (3).png", Some((1920, 1080))),
            (r"C:\Users\Me\Pictures\gone.jpg", None),
        ];
        let c = classify(&items, &roots(), None);
        assert_eq!(c, [ImgClass::Photo, ImgClass::Graphic, ImgClass::Screenshot, ImgClass::Photo]);
    }

    #[test]
    fn an_asset_folder_takes_its_big_images_with_it() {
        let mut owned: Vec<(String, Option<(u32, u32)>)> =
            (0..20).map(|i| (format!(r"C:\Users\Me\Documents\Game\graphics\logos\{i}.png"), Some((180, 180)))).collect();
        owned.push((r"C:\Users\Me\Documents\Game\graphics\backgrounds\stadium.png".into(), Some((1920, 1080))));
        owned.push((r"C:\Users\Me\Documents\scan.jpg".into(), Some((2480, 3508))));
        let items: Vec<(&str, Option<(u32, u32)>)> = owned.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let c = classify(&items, &roots(), None);
        assert!(c[..20].iter().all(|x| *x == ImgClass::Graphic));
        assert_eq!(c[20], ImgClass::Graphic, "a screen-sized background inside a graphics pack is not a screenshot");
        assert_eq!(c[21], ImgClass::Photo, "the indexed root itself is never judged as one folder");
    }

    #[test]
    fn a_few_small_files_do_not_condemn_a_folder() {
        let mut owned: Vec<(String, Option<(u32, u32)>)> =
            (0..5).map(|i| (format!(r"C:\Users\Me\Pictures\Trip\thumb{i}.png"), Some((100, 100)))).collect();
        owned.push((r"C:\Users\Me\Pictures\Trip\beach.jpg".into(), Some((4000, 3000))));
        let items: Vec<(&str, Option<(u32, u32)>)> = owned.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let c = classify(&items, &roots(), None);
        assert_eq!(c[5], ImgClass::Photo, "below ASSET_DIR_MIN images the folder mix is not trusted");
        // mostly big pictures: the folder stays personal however many there are
        let mut owned: Vec<(String, Option<(u32, u32)>)> =
            (0..30).map(|i| (format!(r"C:\Users\Me\Pictures\Trip\p{i}.jpg"), Some((4000, 3000)))).collect();
        owned.extend((0..10).map(|i| (format!(r"C:\Users\Me\Pictures\Trip\t{i}.png"), Some((100, 100)))));
        let items: Vec<(&str, Option<(u32, u32)>)> = owned.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let c = classify(&items, &roots(), None);
        assert!(c[..30].iter().all(|x| *x == ImgClass::Photo));
        assert!(c[30..].iter().all(|x| *x == ImgClass::Graphic));
    }

    #[test]
    fn outside_every_root_the_first_two_components_are_the_floor() {
        let owned: Vec<(String, Option<(u32, u32)>)> =
            (0..20).map(|i| (format!(r"D:\Stuff\{i}.png"), Some((16, 16)))).chain([(r"D:\Stuff\big.jpg".to_string(), Some((3000, 2000)))]).collect();
        let items: Vec<(&str, Option<(u32, u32)>)> = owned.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        let c = classify(&items, &roots(), None);
        assert_eq!(c[20], ImgClass::Photo, r"D:\Stuff is the floor, not a judged folder");
    }
}
