//! Directories that hold derivatives and caches rather than photographs.
//!
//! Lightroom stores its previews in `<catalog> Previews.lrdata` and its smart
//! previews in `<catalog> Smart Previews.lrdata`. Both are directory trees
//! containing thousands of small DNG and JPEG files, one per photo, nested
//! several levels deep under hex-named folders.
//!
//! A recursive scan that walks into one of those will present a user's entire
//! preview cache as if it were their library, take a long time doing it, and
//! bury the real photographs. The same applies to Recycle Bin and system
//! folders that happen to sit on a photo drive.
//!
//! Matching is by directory name only, so nothing here can hide a real photo:
//! the worst case is skipping a folder someone deliberately named `.lrdata`.

use std::path::Path;

/// Suffixes belonging to catalog sidecar trees, matched case-insensitively.
const IGNORED_SUFFIXES: &[&str] = &[
    ".lrdata",      // Lightroom previews and smart previews
    ".lrcat-data",  // Lightroom catalog support data
    ".cosessiondb", // Capture One session database
    ".photoslibrary", // Apple Photos package
];

/// Exact directory names that never contain a user's own photographs.
const IGNORED_NAMES: &[&str] = &[
    "$RECYCLE.BIN",
    "System Volume Information",
    ".git",
    ".Trashes",
    "#recycle", // Synology NAS
    "@eaDir",   // Synology NAS thumbnail store
    ".blitzraw-previews", // our own rendered previews, see preview_cache
    ".darkroom-previews", // and what those were called before the rename
];

/// Whether a directory of this name should be skipped during a library scan.
pub fn is_ignored_directory_name(name: &str) -> bool {
    if IGNORED_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return true;
    }

    let lowered = name.to_ascii_lowercase();
    IGNORED_SUFFIXES.iter().any(|s| lowered.ends_with(s))
}

/// Whether a walked entry should be descended into.
///
/// Returns `true` for every file, so callers can apply this directly as a
/// `WalkDir::filter_entry` predicate without dropping images. The root of a
/// scan is always kept: if someone deliberately opens one of these folders,
/// showing them its contents is the correct response to an explicit request.
pub fn should_visit(path: &Path, depth: usize, is_dir: bool) -> bool {
    if !is_dir || depth == 0 {
        return true;
    }

    path.file_name()
        .and_then(|n| n.to_str())
        .map(|name| !is_ignored_directory_name(name))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_lightroom_preview_trees() {
        assert!(is_ignored_directory_name("Lightroom-v15 Previews.lrdata"));
        assert!(is_ignored_directory_name(
            "Lightroom-v15 Smart Previews.lrdata"
        ));
        assert!(is_ignored_directory_name("Catalog.lrcat-data"));
    }

    #[test]
    fn skips_system_and_nas_clutter() {
        assert!(is_ignored_directory_name("$RECYCLE.BIN"));
        assert!(is_ignored_directory_name("system volume information"));
        assert!(is_ignored_directory_name("@eaDir"));
    }

    #[test]
    fn keeps_ordinary_shoot_folders() {
        assert!(!is_ignored_directory_name("RAW"));
        assert!(!is_ignored_directory_name("2026-07-10-Session"));
        assert!(!is_ignored_directory_name("BestOf"));
        assert!(!is_ignored_directory_name("00-Lightroom"));
    }

    #[test]
    fn files_are_always_visited() {
        let path = Path::new(r"C:\photos\Previews.lrdata");
        assert!(should_visit(path, 3, false));
    }

    #[test]
    fn an_explicitly_opened_folder_is_never_pruned() {
        let path = Path::new(r"C:\photos\Previews.lrdata");
        assert!(should_visit(path, 0, true));
        assert!(!should_visit(path, 1, true));
    }
}
