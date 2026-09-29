//! Support for merging many bracket stacks in one go.
//!
//! The merge itself reuses the existing `merge_hdr` and `save_hdr` commands
//! rather than reimplementing them. Those two share a single result slot in
//! application state: `merge_hdr` fills it and `save_hdr` takes it. So a queue
//! has to run strictly one at a time, or a second merge would overwrite the
//! first result before it was written to disk. The queue lives in the front
//! end where that ordering is easy to see.
//!
//! What is needed here is the one thing the front end cannot answer: whether a
//! stack has already been merged. `save_hdr` overwrites without asking, which
//! is harmless for a single deliberate merge but not for a bulk run repeated
//! over a folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Extensions `save_hdr` writes today.
///
/// `dng` for a merge of raw frames and `png` for anything that was never float,
/// which is what the two branches of `save_hdr` produce.
///
/// **`tiff` is deliberately absent.** It is what merges used to be written as,
/// and a stack holding only one of those has not been merged into the format
/// this app now writes, so re-merging it is the whole point rather than
/// something to protect against. Keeping it here told a shoot of 32 brackets
/// that all 32 were already done and skipped every one of them.
///
/// The guard still does its job: `save_hdr` overwrites without asking, and a
/// re-merge writes `_Hdr.dng`, which leaves any `_Hdr.tiff` beside it alone.
const HDR_OUTPUT_EXTENSIONS: &[&str] = &["dng", "png"];

/// Mirrors the naming `save_hdr` uses: the first frame's stem plus `_Hdr`.
fn candidate_outputs(first_path: &str) -> Vec<PathBuf> {
    let source = first_path.split("?vc=").next().unwrap_or(first_path);
    let path = Path::new(source);

    let (Some(parent), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str()))
    else {
        return Vec::new();
    };

    HDR_OUTPUT_EXTENSIONS
        .iter()
        .map(|ext| parent.join(format!("{stem}_Hdr.{ext}")))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrOutputStatus {
    pub first_path: String,
    /// True when a merge of this stack has already been written.
    pub exists: bool,
    /// The file found, so the UI can name it if asked.
    pub existing_path: Option<String>,
}

/// Reports which stacks already have a merged result on disk.
///
/// Takes the first frame of each stack, since that is what `save_hdr` names
/// its output after.
#[tauri::command]
pub fn hdr_outputs_present(first_paths: Vec<String>) -> Vec<HdrOutputStatus> {
    first_paths
        .into_iter()
        .map(|first_path| {
            let existing = candidate_outputs(&first_path)
                .into_iter()
                .find(|p| p.is_file());
            HdrOutputStatus {
                first_path,
                exists: existing.is_some(),
                existing_path: existing.map(|p| p.to_string_lossy().into_owned()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shoot folder the naming tests use, spelled the way this system
    /// spells a path. A Windows path on a Mac is one long file name.
    fn shoot_folder() -> &'static Path {
        Path::new(if cfg!(windows) { r"C:\shoot\RAW" } else { "/shoot/RAW" })
    }

    fn in_shoot(name: &str) -> String {
        shoot_folder().join(name).to_string_lossy().into_owned()
    }

    #[test]
    fn a_shoot_merged_before_the_dng_change_is_offered_again() {
        let dir = std::env::temp_dir().join("blitzraw-bulk-hdr-legacy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // What the working set looks like right now: merged once, as TIFFs.
        let first = dir.join("_DSC3473.dng");
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(dir.join("_DSC3473_Hdr.tiff"), b"x").unwrap();

        let statuses = hdr_outputs_present(vec![first.to_string_lossy().to_string()]);
        assert!(
            !statuses[0].exists,
            "an old TIFF merge must not count as done, or the shoot can never be re-merged"
        );

        // And once it has been, it is done and stays done.
        std::fs::write(dir.join("_DSC3473_Hdr.dng"), b"x").unwrap();
        let statuses = hdr_outputs_present(vec![first.to_string_lossy().to_string()]);
        assert!(statuses[0].exists, "the DNG merge counts");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn candidates_follow_the_naming_save_hdr_uses() {
        let outputs = candidate_outputs(&in_shoot("_DSC1794.NEF"));
        let names: Vec<String> = outputs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(
            names.contains(&"_DSC1794_Hdr.dng".to_string()),
            "a merge of raw frames"
        );
        assert!(
            names.contains(&"_DSC1794_Hdr.png".to_string()),
            "a merge of anything not float"
        );
        assert!(
            !names.contains(&"_DSC1794_Hdr.tiff".to_string()),
            "tiff is what merges used to be, and a stack holding only one has not been merged              into the format written today, so it must still be offered"
        );
    }

    #[test]
    fn candidates_sit_beside_the_source_frame() {
        let outputs = candidate_outputs(&in_shoot("_DSC1794.NEF"));
        assert!(!outputs.is_empty());
        assert!(outputs.iter().all(|p| p.parent() == Some(shoot_folder())));
    }

    #[test]
    fn a_virtual_copy_resolves_to_the_file_it_copies() {
        let outputs = candidate_outputs(&format!("{}?vc=abc123", in_shoot("_DSC1794.NEF")));
        let names: Vec<String> = outputs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(names.contains(&"_DSC1794_Hdr.png".to_string()));
    }

    #[test]
    fn a_dotted_stem_is_kept_whole() {
        let outputs = candidate_outputs(&in_shoot("shoot.02.raw.NEF"));
        let names: Vec<String> = outputs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(
            names.contains(&"shoot.02.raw_Hdr.png".to_string()),
            "got {names:?}"
        );
    }

    #[test]
    fn a_path_without_a_parent_yields_nothing_rather_than_panicking() {
        assert!(candidate_outputs("").is_empty());
    }

    #[test]
    fn nothing_on_disk_reports_as_absent() {
        let status = hdr_outputs_present(vec![r"C:\definitely\not\here\_DSC0001.NEF".to_string()]);
        assert_eq!(status.len(), 1);
        assert!(!status[0].exists);
        assert!(status[0].existing_path.is_none());
    }
}
