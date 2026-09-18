use memmap2::{Mmap, MmapOptions};
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageBuffer, Luma};
use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sysinfo::Disks;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::AppState;
use crate::PendingMetadata;
#[cfg(target_os = "android")]
use crate::android_integration::*;
use crate::app_settings::*;
use crate::exif_processing;
use crate::formats::{is_raw_file, is_supported_image_file};
use crate::gpu_processing;
use crate::image_loader;
use crate::image_processing::GpuContext;
use crate::image_processing::{
    Crop, ImageMetadata, apply_coarse_rotation, apply_cpu_default_raw_processing, apply_crop,
    apply_flip, apply_geometry_warp, apply_rotation, auto_results_to_json,
    get_all_adjustments_from_json, perform_auto_analysis,
};
use crate::mask_generation::MaskDefinition;
use crate::preset_converter;
use crate::tagging::COLOR_TAG_PREFIX;

fn resolve_thumbnail_cache_dir(app_handle: &AppHandle) -> std::result::Result<PathBuf, String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");
    if !thumb_cache_dir.exists() {
        fs::create_dir_all(&thumb_cache_dir).map_err(|e| e.to_string())?;
    }
    Ok(thumb_cache_dir)
}

fn emit_thumbnail_cache_setup_error(app_handle: &AppHandle, path: &str, reason: &str) {
    let _ = app_handle.emit(
        "thumbnail-generation-error",
        serde_json::json!({ "path": path, "reason": reason }),
    );
}

// ========= BLITZRAW: one thumbnail per photo, overwritten in place =========
//
// The cache name used to be a hash of the path, the file's modified time and
// the adjustments, so every time an adjustment moved a new thumbnail was
// written under a new name and the previous one stayed for ever. Measured after
// a week on one machine: 4,744 files, 162 MB, for a library of a few hundred
// photos. Nothing could clean up, because the name could not say which photo it
// belonged to.
//
// So a photo is given a name for its thumbnail once, at random, and that name
// is written into its sidecar. A new thumbnail overwrites the old one. No
// folders, no index, nothing to search, nothing to delete. The number of files
// in the cache is the number of photos ever looked at, and it stops there.
//
// # Why the name is not made from the path
//
// Because a path changes. A shoot moves from the working drive to the archive
// and then to a backup, and
// files get renamed. Anything derived from the path would give every photo a
// new thumbnail on each of those and leave the old one behind, which is the
// original fault wearing a different hat.
//
// The sidecar travels with the photo: it is written beside it, renamed with it
// and copied with it. So a name kept in the sidecar survives everything a path
// does not. Measured before relying on it: 1,919 photos in the working set and
// 2,166 sidecars, so a photo without one is not a case that arises in practice,
// and one is written the first time a thumbnail is made in any case.
//
// # Knowing when one is out of date
//
// By its modified time, against the photo and against the photo's sidecar. The
// sidecar is where adjustments live, so it moves whenever an edit does, and the
// photo itself covers the file being replaced.
//
// This costs nothing extra because writing that sidecar already regenerates the
// thumbnail: `save_metadata_and_update_thumbnail` does exactly that, in that
// order. A thumbnail is therefore normally newer than its sidecar, and one that
// is not really may be wrong, which is exactly when it should be made again.

/// How long a thumbnail name is, in hex characters. 128 bits: two photos in one
/// library colliding is not a thing that happens.
const THUMBNAIL_ID_LEN: usize = 32;

/// The name of this photo's thumbnail, giving it one if it does not have one.
///
/// Reads the sidecar, and writes the name back into it the first time. Failing
/// to write is not fatal: the thumbnail is still made and still shown, it is
/// just made again next time, which is the behaviour this replaces rather than
/// a regression.
fn thumbnail_id_for(path_str: &str) -> String {
    let (_, sidecar_path) = parse_virtual_path(path_str);

    let usable =
        |id: &String| id.len() == THUMBNAIL_ID_LEN && id.chars().all(|c| c.is_ascii_hexdigit());

    // Read first, without the lock, because this is asked on every render of
    // every photo and almost always already has an answer.
    if let Some(existing) = crate::exif_processing::load_sidecar(&sidecar_path)
        .thumbnail_id
        .filter(usable)
    {
        return existing;
    }

    // Naming a photo's thumbnail is a change to its sidecar like any other, so
    // it goes through the same door. It used to read and write on its own, from
    // inside a thumbnail render, which is exactly when an edit is most likely to
    // be writing the same file: naming a thumbnail could eat an exposure.
    let named = crate::sidecar::update(&sidecar_path, |meta| {
        // Checked again in here. Another render of the same photo may have
        // named it between the read above and this lock.
        if let Some(existing) = meta.thumbnail_id.as_ref().filter(|id| usable(id)) {
            return Some((existing.clone(), false));
        }
        let fresh = uuid::Uuid::new_v4().simple().to_string();
        meta.thumbnail_id = Some(fresh.clone());
        Some((fresh, true))
    });

    match named {
        Ok(Some((id, _))) => id,
        Ok(None) => uuid::Uuid::new_v4().simple().to_string(),
        Err(e) => {
            log::warn!(
                "Could not record the thumbnail name for {path_str} in {}: {e}",
                sidecar_path.display()
            );
            uuid::Uuid::new_v4().simple().to_string()
        }
    }
}

/// Where one photo's thumbnail lives. Always this, whatever has been done to
/// the photo and wherever the photo has been moved to.
fn thumbnail_path_for(cache_dir: &Path, path_str: &str) -> PathBuf {
    cache_dir.join(format!("{}.jpg", thumbnail_id_for(path_str)))
}

// ======== BLITZRAW: a thumbnail is stale when the picture would differ ========
/// Bump to discard every cached thumbnail at once.
const THUMBNAIL_KEY_VERSION: u32 = 1;

/// Everything that decides what a thumbnail looks like, as one short string.
///
/// # Why not the file's modification time
///
/// The rule used to be "older than the photo or older than its sidecar means
/// stale". A sidecar holds far more than the picture: a rating, a colour label,
/// tags, cached EXIF, the name of the thumbnail itself, the edit history. Any
/// write for any of those made every cached picture of that photo worthless,
/// and a folder scan writes a lot of them. That is how a start-up came to
/// re-render a whole shoot without a single adjustment having changed.
///
/// This asks the only question that matters: **would rendering it again produce
/// a different picture?** The answer is the photo's own bytes, its adjustments,
/// and the settings that change how a decode is turned into a picture. Nothing
/// else goes in, so a star, a tag or a note about the camera's own rating costs
/// nothing.
///
/// The list of settings below has to be kept in step with what
/// `generate_thumbnail_data` and the decode beneath it actually read. A setting
/// missing from here shows up as a thumbnail that will not update when that
/// setting is changed; a setting that does not belong here shows up as a folder
/// that re-renders for no reason. Both are visible, and neither is silent.
fn thumbnail_freshness_key(
    path_str: &str,
    adjustments: &Value,
    settings: &AppSettings,
) -> Option<String> {
    let (source, _) = parse_virtual_path(path_str);
    let meta = fs::metadata(&source).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    let is_raw = crate::formats::is_raw_file(path_str);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&THUMBNAIL_KEY_VERSION.to_le_bytes());

    // The photo itself. A re-merged HDR written over its own name changes both.
    hasher.update(&modified.to_le_bytes());
    hasher.update(&meta.len().to_le_bytes());

    // What has been done to it.
    hasher.update(&serde_json::to_vec(adjustments).unwrap_or_default());

    // And how a decode becomes a picture.
    hasher.update(&settings.thumbnail_resolution.unwrap_or(720).to_le_bytes());
    hasher.update(&[settings.always_decode_raw_thumbnails.unwrap_or(false) as u8]);
    hasher.update(
        &settings
            .raw_highlight_compression
            .unwrap_or(2.5)
            .to_le_bytes(),
    );
    hasher.update(settings.linear_raw_mode.as_bytes());
    hasher.update(
        &settings
            .raw_preprocessing_color_nr
            .unwrap_or(0.5)
            .to_le_bytes(),
    );
    hasher.update(
        &settings
            .raw_preprocessing_sharpening
            .unwrap_or(0.35)
            .to_le_bytes(),
    );
    hasher.update(&[settings.apply_preprocessing_to_non_raws.unwrap_or(false) as u8]);
    hasher.update(
        &crate::image_processing::resolve_tonemapper_override(settings, is_raw)
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );

    Some(hasher.finalize().to_hex()[..16].to_string())
}

/// Where a thumbnail's key is written. Beside it, sharing its name.
///
/// A separate file rather than part of the name, because the name is deliberately
/// fixed for the life of the photo: the front end holds the address of a
/// thumbnail and a changing address would orphan every cached picture on every
/// edit. The key travels beside it instead.
fn thumbnail_key_path(cache_path: &Path) -> PathBuf {
    cache_path.with_extension("key")
}

fn read_thumbnail_key(cache_path: &Path) -> Option<String> {
    fs::read_to_string(thumbnail_key_path(cache_path))
        .ok()
        .map(|held| held.trim().to_string())
        .filter(|held| !held.is_empty())
}

/// The old rule, kept for one purpose only.
///
/// Every thumbnail written before keys existed has no key beside it. Treating
/// those as stale would re-render an entire library once, for nothing. So an
/// unkeyed thumbnail is judged the old way, and if it passes it is given the
/// key it should have had. After that it is never asked this question again.
fn thumbnail_predates_its_photo(cache_path: &Path, path_str: &str) -> bool {
    let Ok(thumb_time) = fs::metadata(cache_path).and_then(|m| m.modified()) else {
        return true;
    };
    let (source_path, sidecar_path) = parse_virtual_path(path_str);

    for other in [source_path.as_path(), sidecar_path.as_path()] {
        if let Ok(time) = fs::metadata(other).and_then(|m| m.modified())
            && time > thumb_time
        {
            return true;
        }
    }
    false
}

/// Whether the cached thumbnail is the picture these adjustments would produce.
fn thumbnail_is_current(
    cache_path: &Path,
    path_str: &str,
    adjustments: &Value,
    settings: &AppSettings,
) -> bool {
    let Some(wanted) = thumbnail_freshness_key(path_str, adjustments, settings) else {
        // The photo itself could not be read. Nothing useful can be said, and
        // re-rendering it would fail for the same reason.
        return true;
    };

    match read_thumbnail_key(cache_path) {
        Some(held) => held == wanted,
        None => {
            let usable = !thumbnail_predates_its_photo(cache_path, path_str);
            if usable {
                let _ = fs::write(thumbnail_key_path(cache_path), &wanted);
            }
            usable
        }
    }
}
// ====== BLITZRAW END: a thumbnail is stale when the picture would differ ======

/// Writes a thumbnail over whatever was there.
///
/// Beside and renamed, so an interrupted write can never leave a half-written
/// file that would later be served as a thumbnail.
fn write_thumbnail(cache_path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temp = cache_path.with_extension("jpg.part");
    fs::write(&temp, data)?;
    fs::rename(&temp, cache_path)
}

/// Clears out thumbnails from every layout this cache has had before.
///
/// Three of them now: the original flat hashes, a folder per photo, and names
/// built from the file name and the path. None can be reached any more and none
/// can be attributed to a photo, so they can only be removed wholesale. Anything
/// the current scheme wrote is exactly thirty-two hex characters, which is what
/// tells them apart. Returns how many went, so the log can say. Costs one
/// directory listing on every start after the first.
pub fn sweep_legacy_thumbnails(app_handle: &AppHandle) -> usize {
    let Ok(dir) = get_thumb_cache_dir(app_handle) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return 0;
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            if fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
            continue;
        }

        let written_by_this_scheme =
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    stem.len() == THUMBNAIL_ID_LEN && stem.chars().all(|c| c.is_ascii_hexdigit())
                });

        if !written_by_this_scheme
            && path.extension().and_then(|e| e.to_str()) == Some("jpg")
            && fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }

    if removed > 0 {
        log::info!("Removed {removed} thumbnails left over from an older cache layout");
    }
    removed
}
// ======= BLITZRAW END: one thumbnail per photo, overwritten in place =======

#[cfg(test)]
mod thumbnail_name_tests {
    use super::*;

    fn temp_photo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("blitzraw-thumb-name");
        let _ = fs::create_dir_all(&dir);
        let photo = dir.join(name);
        let _ = fs::write(&photo, b"not really a photo");
        photo
    }

    /// The name is fixed on purpose, so the version has to be what moves.
    ///
    /// Checked by rewriting the file rather than assumed: with the version
    /// removed the URL is the same string both times, which is exactly what
    /// left the grid showing the picture from before the edit.
    #[test]
    fn rewriting_a_thumbnail_gives_it_a_new_version() {
        let dir = std::env::temp_dir().join("blitzraw-thumb-version");
        let _ = fs::create_dir_all(&dir);
        let thumb = dir.join("version.jpg");
        let name = thumb.to_string_lossy().to_string();

        let _ = fs::write(&thumb, b"the picture before the edit");
        let before = thumbnail_version(&name);
        assert!(before > 0, "a file that exists has a version");

        // A rewrite is a temp file and a rename, so it always lands on a fresh
        // timestamp. This only has to outrun the clock's resolution.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = fs::write(&thumb, b"the picture after the edit");
        let after = thumbnail_version(&name);

        assert_ne!(before, after, "a rewritten thumbnail must not keep its URL");
        assert_eq!(
            after,
            thumbnail_version(&name),
            "and one nobody touched must keep it, or nothing stays cached"
        );

        let _ = fs::remove_file(&thumb);
    }

    /// A thumbnail that is not there yet reports no version at all, which the
    /// front end reads as "no version" and not as version zero.
    #[test]
    fn a_thumbnail_that_is_not_there_has_no_version() {
        let missing = std::env::temp_dir().join("blitzraw-thumb-version/not-written.jpg");
        assert_eq!(thumbnail_version(&missing.to_string_lossy()), 0);
    }

    #[test]
    fn a_photo_keeps_the_same_thumbnail_name_for_ever() {
        let photo = temp_photo("keeps.nef");
        let path = photo.to_string_lossy().to_string();
        let (_, sidecar) = parse_virtual_path(&path);
        let _ = fs::remove_file(&sidecar);

        let first = thumbnail_id_for(&path);
        let second = thumbnail_id_for(&path);
        assert_eq!(first, second, "asking twice must not give two names");
        assert_eq!(first.len(), THUMBNAIL_ID_LEN);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));

        // And it is in the sidecar, which is what carries it across a move.
        let written = crate::exif_processing::load_sidecar(&sidecar);
        assert_eq!(written.thumbnail_id.as_deref(), Some(first.as_str()));

        let _ = fs::remove_file(&photo);
        let _ = fs::remove_file(&sidecar);
    }

    #[test]
    fn two_photos_get_two_names() {
        let a = temp_photo("one.nef").to_string_lossy().to_string();
        let b = temp_photo("two.nef").to_string_lossy().to_string();
        for p in [&a, &b] {
            let (_, sidecar) = parse_virtual_path(p);
            let _ = fs::remove_file(&sidecar);
        }
        assert_ne!(thumbnail_id_for(&a), thumbnail_id_for(&b));
    }

    #[test]
    fn a_name_that_makes_no_sense_is_replaced() {
        // A sidecar hand-edited, or written by something else, should not send
        // the cache looking for a file called whatever happens to be in there.
        let photo = temp_photo("nonsense.nef");
        let path = photo.to_string_lossy().to_string();
        let (_, sidecar) = parse_virtual_path(&path);

        let mut metadata = crate::exif_processing::load_sidecar(&sidecar);
        metadata.thumbnail_id = Some("../../etc/passwd".to_string());
        fs::write(&sidecar, serde_json::to_string_pretty(&metadata).unwrap()).unwrap();

        let id = thumbnail_id_for(&path);
        assert_ne!(id, "../../etc/passwd");
        assert_eq!(id.len(), THUMBNAIL_ID_LEN);
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "a name is only ever hex"
        );

        let _ = fs::remove_file(&photo);
        let _ = fs::remove_file(&sidecar);
    }

    #[test]
    fn a_virtual_copy_has_its_own_thumbnail() {
        let photo = temp_photo("copied.nef");
        let original = photo.to_string_lossy().to_string();
        let copy = format!("{original}?vc=1");
        for p in [&original, &copy] {
            let (_, sidecar) = parse_virtual_path(p);
            let _ = fs::remove_file(&sidecar);
        }
        assert_ne!(
            thumbnail_id_for(&original),
            thumbnail_id_for(&copy),
            "a copy is a different picture and needs a different thumbnail"
        );
    }
}

struct ImageFileMetadata {
    is_edited: bool,
    tags: Option<Vec<String>>,
    rating: u8,
    is_raw: bool,
}

/// Writes back only what the XMP syncs above are allowed to move.
///
/// BLITZRAW: a folder scan reads every sidecar and must not write back the
/// whole copy it read, because an edit saved since would be undone by it. Three
/// fields, then, and no more.
///
/// **`camera_rating` is one of the three, and leaving it out was a bug that
/// invalidated every thumbnail in a folder on every start-up.**
/// `sync_metadata_from_embedded_xmp` asks the photo for the stars set on the
/// camera exactly once, and the only thing that stops it asking again is
/// `camera_rating` being on disk. Written only in memory, it was asked again on
/// the next scan, reported as a change again, and the sidecar was rewritten
/// again. A rewritten sidecar is newer than its thumbnail, and a thumbnail
/// older than its sidecar is discarded, so every photo in the folder was
/// re-rendered on every launch, for ever.
///
/// Returns `None` when the three are already what is on disk, which is the
/// usual case and writes nothing at all. That is the difference between a scan
/// that costs one read per photo and one that costs a read, a write and a
/// render.
fn write_synced_fields(sidecar_path: &Path, synced: &ImageMetadata) -> std::io::Result<Option<()>> {
    crate::sidecar::update(sidecar_path, |on_disk| {
        if on_disk.rating == synced.rating
            && on_disk.tags == synced.tags
            && on_disk.camera_rating == synced.camera_rating
        {
            return None;
        }
        on_disk.rating = synced.rating;
        on_disk.tags = synced.tags.clone();
        on_disk.camera_rating = synced.camera_rating;
        Some(())
    })
}

fn resolve_image_metadata(
    image_path: &Path,
    sidecar_path: &Path,
    enable_xmp_sync: bool,
    settings: &AppSettings,
) -> ImageFileMetadata {
    let mut metadata = crate::exif_processing::load_sidecar(sidecar_path);

    let mut changed = false;
    if enable_xmp_sync {
        changed |= sync_metadata_from_xmp(image_path, &mut metadata);

        // BLITZRAW: the camera's own stars, asked for once per photo.
        changed |= sync_metadata_from_embedded_xmp(image_path, &mut metadata);
    }

    if changed {
        let _ = write_synced_fields(sidecar_path, &metadata);
    }

    let is_raw = crate::formats::is_raw_file(image_path);
    let tm_override = crate::image_processing::resolve_tonemapper_override(settings, is_raw);
    let is_edited =
        crate::image_processing::is_image_edited(&metadata.adjustments, is_raw, tm_override);
    ImageFileMetadata {
        is_edited,
        tags: metadata.tags,
        rating: metadata.rating,
        is_raw,
    }
}

fn emit_image_metadata_loaded(
    app_handle: &AppHandle,
    path: &str,
    rating: u8,
    is_edited: bool,
    tags: &Option<Vec<String>>,
) {
    let _ = app_handle.emit(
        "image-metadata-loaded",
        serde_json::json!({ "path": path, "rating": rating, "is_edited": is_edited, "tags": tags }),
    );
}

fn enqueue_metadata(
    app_handle: &AppHandle,
    virtual_path: String,
    image_path: PathBuf,
    sidecar_path: PathBuf,
) {
    let state = app_handle.state::<crate::AppState>();
    let manager = &state.metadata_manager;

    let mut pending = manager.pending.lock().unwrap();
    if !pending.insert(sidecar_path.clone()) {
        return;
    }
    drop(pending);

    manager.queue.lock().unwrap().push_back(PendingMetadata {
        virtual_path,
        image_path,
        sidecar_path,
    });
    manager.cvar.notify_one();
}

// Not compute-heavy — these threads mostly block waiting on iCloud to
// materialize a file, not burning CPU — so a small fixed pool is enough and
// doesn't need a user-facing setting the way thumbnail_worker_threads does.
const METADATA_WORKER_THREADS: usize = 4;

pub fn start_metadata_workers(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<crate::AppState>();
    let manager = state.metadata_manager.clone();

    for _ in 0..METADATA_WORKER_THREADS {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();

        std::thread::spawn(move || {
            loop {
                let item = {
                    let mut queue = manager_clone.queue.lock().unwrap();
                    while queue.is_empty() {
                        queue = manager_clone.cvar.wait(queue).unwrap();
                    }
                    queue.pop_front().unwrap()
                };

                let settings = load_settings(app_clone.clone()).unwrap_or_default();
                let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

                let metadata = resolve_image_metadata(
                    &item.image_path,
                    &item.sidecar_path,
                    enable_xmp_sync,
                    &settings,
                );

                emit_image_metadata_loaded(
                    &app_clone,
                    &item.virtual_path,
                    metadata.rating,
                    metadata.is_edited,
                    &metadata.tags,
                );

                manager_clone
                    .pending
                    .lock()
                    .unwrap()
                    .remove(&item.sidecar_path);
            }
        });
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub adjustments: Value,
    #[serde(rename = "includeMasks", skip_serializing_if = "Option::is_none")]
    pub include_masks: Option<bool>,
    #[serde(
        rename = "includeCropTransform",
        skip_serializing_if = "Option::is_none"
    )]
    pub include_crop_transform: Option<bool>,
    #[serde(rename = "presetType", skip_serializing_if = "Option::is_none")]
    pub preset_type: Option<String>,
}

#[derive(Serialize)]
struct ExportPresetFile<'a> {
    creator: &'a str,
    presets: &'a [PresetItem],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PresetFolder {
    pub id: String,
    pub name: String,
    pub children: Vec<Preset>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum PresetItem {
    Preset(Preset),
    Folder(PresetFolder),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PresetFile {
    pub presets: Vec<PresetItem>,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PresetImportFailure {
    pub file_name: String,
    pub error: String,
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PresetImportResult {
    pub presets: Vec<PresetItem>,
    pub failures: Vec<PresetImportFailure>,
}

#[derive(Debug)]
pub enum ReadFileError {
    Io(std::io::Error),
    Locked,
    Empty,
    NotFound,
    Invalid,
}

impl fmt::Display for ReadFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadFileError::Io(err) => write!(f, "IO error: {}", err),
            ReadFileError::Locked => write!(f, "File is locked"),
            ReadFileError::Empty => write!(f, "File is empty"),
            ReadFileError::NotFound => write!(f, "File not found"),
            ReadFileError::Invalid => write!(f, "Invalid file"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageFile {
    pub path: String,
    modified: u64,
    is_edited: bool,
    rating: u8,
    tags: Option<Vec<String>>,
    exif: Option<HashMap<String, String>>,
    is_virtual_copy: bool,
    is_cloud_placeholder: bool,
    is_raw: bool,
    group_id: Option<String>,
    /// Which user-confirmed stack this frame belongs to, if any. Distinct from
    /// `group_id`: that ties files of one capture together, this ties several
    /// captures together. See `crate::stacks`.
    stack_id: Option<String>,
    /// Whether this is the frame shown when its stack is closed. False for
    /// stacks that never had a leader chosen, where display order decides.
    is_stack_leader: bool,
}

/// Tags each file with the stack it belongs to, reading one record per folder.
fn assign_stack_ids(files: &mut [ImageFile]) {
    let mut by_folder: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();
    let mut leaders: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();

    for file in files.iter_mut() {
        let (source_path, _) = parse_virtual_path(&file.path);
        let Some(parent) = source_path.parent() else {
            continue;
        };
        let Some(name) = source_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        let lookup = by_folder
            .entry(parent.to_path_buf())
            .or_insert_with(|| crate::stacks::stack_ids_by_file_name(parent));

        file.stack_id = lookup.get(name).cloned();
        file.is_stack_leader = leaders
            .entry(parent.to_path_buf())
            .or_insert_with(|| crate::stacks::leaders_by_stack_id(parent))
            .get(file.stack_id.as_deref().unwrap_or_default())
            .is_some_and(|leader| leader == name);
    }
}

fn make_group_key(source_path: &Path) -> String {
    let parent = source_path.parent().unwrap_or(Path::new(""));
    let stem = source_path.file_stem().unwrap_or_default();
    format!("{}/{}", parent.to_string_lossy(), stem.to_string_lossy())
}

fn assign_group_ids(files: &mut [ImageFile], settings: &crate::app_settings::AppSettings) {
    let require_matching_exif = settings.require_matching_exif.unwrap_or(false);
    let group_edited_files = settings.group_edited_files.unwrap_or(true);

    #[derive(Clone)]
    struct Candidate {
        index: usize,
        source_path: PathBuf,
        key: String,
    }

    let candidates: Vec<Candidate> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.is_virtual_copy && (group_edited_files || !file.is_edited))
        .map(|(index, file)| {
            let (source_path, _) = parse_virtual_path(&file.path);
            let key = make_group_key(&source_path);
            Candidate {
                index,
                source_path,
                key,
            }
        })
        .collect();

    let mut stem_groups: HashMap<String, Vec<Candidate>> = HashMap::new();
    for candidate in candidates {
        stem_groups
            .entry(candidate.key.clone())
            .or_default()
            .push(candidate);
    }

    if require_matching_exif {
        let groupable_paths: Vec<PathBuf> = stem_groups
            .values()
            .filter(|candidates| candidates.len() >= 2)
            .flat_map(|candidates| candidates.iter().map(|c| c.source_path.clone()))
            .collect();
        let exif_dates: HashMap<PathBuf, Option<chrono::DateTime<chrono::Utc>>> = groupable_paths
            .par_iter()
            .map(|p| {
                (
                    p.clone(),
                    crate::exif_processing::try_get_exif_creation_date(p),
                )
            })
            .collect();

        stem_groups.retain(|_, candidates| {
            if candidates.len() < 2 {
                return false;
            }
            let first = exif_dates
                .get(&candidates[0].source_path)
                .copied()
                .flatten();
            if first.is_none() {
                return false;
            }
            candidates
                .iter()
                .skip(1)
                .all(|c| exif_dates.get(&c.source_path).copied().flatten() == first)
        });
    } else {
        stem_groups.retain(|_, candidates| candidates.len() >= 2);
    }

    for (key, candidates) in stem_groups {
        for candidate in candidates {
            files[candidate.index].group_id = Some(key.clone());
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImportSettings {
    pub filename_template: String,
    pub organize_by_date: bool,
    pub date_folder_format: String,
    pub delete_after_import: bool,
}

pub fn parse_virtual_path(virtual_path: &str) -> (PathBuf, PathBuf) {
    let (source_path_str, copy_id) = if let Some((base, id)) = virtual_path.rsplit_once("?vc=") {
        (base.to_string(), Some(id.to_string()))
    } else {
        (virtual_path.to_string(), None)
    };

    let source_path = PathBuf::from(source_path_str);

    let sidecar_filename = if let Some(id) = copy_id {
        format!(
            "{}.{}.rrdata",
            source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            &id
        )
    } else {
        format!(
            "{}.rrdata",
            source_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        )
    };

    let sidecar_path = source_path.with_file_name(sidecar_filename);
    (source_path, sidecar_path)
}

#[tauri::command]
pub async fn read_exif_for_paths(
    paths: Vec<String>,
    state: tauri::State<'_, AppState>,
) -> Result<HashMap<String, HashMap<String, String>>, String> {
    let is_hdd = state
        .thumbnail_manager
        .rotational_disk
        .load(Ordering::Relaxed);

    tauri::async_runtime::spawn_blocking(move || {
        let process_path = |virtual_path: &String| {
            let (source_path, _) = parse_virtual_path(virtual_path);
            let source_path_str = source_path.to_string_lossy().to_string();

            let map = if let Some(sidecar_exif) =
                crate::exif_processing::read_rrexif_sidecar(&source_path)
            {
                sidecar_exif
            } else if is_cloud_placeholder(&source_path) {
                HashMap::new()
            } else if let Ok(mmap) = read_file_mapped(&source_path) {
                crate::exif_processing::read_exif_data(&source_path_str, &mmap)
            } else if let Ok(bytes) = fs::read(&source_path) {
                crate::exif_processing::read_exif_data(&source_path_str, &bytes)
            } else {
                HashMap::new()
            };

            if map.is_empty() {
                None
            } else {
                Some((virtual_path.clone(), map))
            }
        };

        let exif_data: HashMap<String, HashMap<String, String>> = if is_hdd {
            paths.iter().filter_map(process_path).collect()
        } else {
            paths.par_iter().filter_map(process_path).collect()
        };

        Ok(exif_data)
    })
    .await
    .unwrap_or_else(|e| Err(format!("Task failed: {}", e)))
}

#[tauri::command]
pub async fn update_exif_fields(
    paths: Vec<String>,
    updates: HashMap<String, String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        paths.par_iter().for_each(|path| {
            let original_path = Path::new(&path);
            let primary_path = crate::exif_processing::get_primary_sidecar_path(original_path);
            let temp_metadata = crate::exif_processing::load_sidecar(&primary_path);

            let mut exif_data = temp_metadata.exif.unwrap_or_else(|| {
                if let Some(existing) = crate::exif_processing::read_rrexif_sidecar(original_path) {
                    existing
                } else if let Ok(mmap) = read_file_mapped(original_path) {
                    crate::exif_processing::read_exif_data_from_bytes(path, &mmap)
                } else if let Ok(bytes) = fs::read(original_path) {
                    crate::exif_processing::read_exif_data_from_bytes(path, &bytes)
                } else {
                    HashMap::new()
                }
            });

            for (k, v) in &updates {
                let trimmed = v.trim();
                if trimmed.is_empty() {
                    exif_data.remove(k);
                } else {
                    exif_data.insert(k.clone(), trimmed.to_string());
                }
            }

            let mut final_metadata = crate::exif_processing::load_sidecar(&primary_path);

            final_metadata.exif = Some(exif_data);
            if let Ok(json) = serde_json::to_string_pretty(&final_metadata) {
                let _ = std::fs::write(&primary_path, json);
            }
        });
        Ok(())
    })
    .await
    .map_err(|e| format!("Task failed: {}", e))?
}

fn match_disk_kind(disks: &Disks, canonical: &Path) -> Option<bool> {
    let mut best_match: Option<(&Path, bool)> = None;

    for disk in disks.list() {
        let mount_point = disk.mount_point();
        if canonical.starts_with(mount_point) {
            let is_longer_match = best_match
                .map(|(current, _)| mount_point.as_os_str().len() > current.as_os_str().len())
                .unwrap_or(true);
            if is_longer_match {
                best_match = Some((mount_point, disk.kind() == sysinfo::DiskKind::HDD));
            }
        }
    }

    best_match.map(|(_, is_hdd)| is_hdd)
}

fn update_rotational_disk_flag(path: &str, app_handle: &AppHandle) {
    let state = app_handle.state::<crate::AppState>();
    let Ok(canonical) = Path::new(path).canonicalize() else {
        return;
    };

    let cached_match = {
        let cache = state.disks_cache.lock().unwrap();
        cache
            .as_ref()
            .and_then(|disks| match_disk_kind(disks, &canonical))
    };

    match cached_match {
        Some(is_hdd) => {
            state
                .thumbnail_manager
                .rotational_disk
                .store(is_hdd, Ordering::Relaxed);
        }
        None => {
            if !state.disks_cache_refreshing.swap(true, Ordering::Relaxed) {
                let refresh_app_handle = app_handle.clone();
                thread::spawn(move || {
                    let disks = Disks::new_with_refreshed_list();
                    let state = refresh_app_handle.state::<crate::AppState>();
                    *state.disks_cache.lock().unwrap() = Some(disks);
                    state.disks_cache_refreshing.store(false, Ordering::Relaxed);
                });
            }
        }
    }
}

#[tauri::command]
pub fn list_images_in_dir(path: String, app_handle: AppHandle) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    update_rotational_disk_flag(&path, &app_handle);

    let entries = fs::read_dir(&path).map_err(|e| e.to_string())?;
    let mut images = Vec::new();
    let mut sidecars_by_filename: HashMap<String, Vec<Option<String>>> = HashMap::new();

    for entry in entries.filter_map(Result::ok) {
        let entry_path = entry.path();
        let file_name = entry
            .file_name()
            .into_string()
            .unwrap_or_else(|os| os.to_string_lossy().into_owned());

        if file_name.ends_with(".rrdata") {
            let base = &file_name[..file_name.len() - 7];

            let (source_filename, copy_id) =
                if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
                    let id = &base[base.len() - 6..];
                    if id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                        (&base[..base.len() - 7], Some(id.to_string()))
                    } else {
                        (base, None)
                    }
                } else {
                    (base, None)
                };

            sidecars_by_filename
                .entry(source_filename.to_string())
                .or_default()
                .push(copy_id);
        } else if is_supported_image_file(&file_name) {
            images.push((file_name, entry_path));
        }
    }

    let tasks: Vec<_> = images
        .into_iter()
        .map(|(file_name, path_buf)| {
            let sidecars = sidecars_by_filename
                .remove(&file_name)
                .unwrap_or_else(|| vec![None]);
            let path_str = path_buf.to_string_lossy().into_owned();
            (path_str, file_name, path_buf, sidecars)
        })
        .collect();

    let mut result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(path_str, file_name, path_buf, sidecars)| {
            let modified = fs::metadata(&path_buf)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_cloud_placeholder = is_cloud_placeholder(&path_buf);

            let mut file_results = Vec::with_capacity(sidecars.len());

            for copy_id_opt in sidecars {
                let (virtual_path, is_virtual_copy, sidecar_filename) = match copy_id_opt {
                    Some(id) => (
                        format!("{}?vc={}", path_str, id),
                        true,
                        format!("{}.{}.rrdata", file_name, id),
                    ),
                    None => (path_str.clone(), false, format!("{}.rrdata", file_name)),
                };

                let sidecar_path = path_buf.with_file_name(sidecar_filename);

                let xmp_is_placeholder = enable_xmp_sync
                    && resolve_xmp_path(&path_buf)
                        .is_some_and(|p| crate::file_management::is_cloud_placeholder(&p));

                let metadata = if crate::file_management::is_cloud_placeholder(&sidecar_path)
                    || xmp_is_placeholder
                {
                    enqueue_metadata(
                        &app_handle,
                        virtual_path.clone(),
                        path_buf.clone(),
                        sidecar_path.clone(),
                    );
                    ImageFileMetadata {
                        is_edited: false,
                        tags: None,
                        rating: 0,
                        is_raw: crate::formats::is_raw_file(&path_buf),
                    }
                } else {
                    resolve_image_metadata(&path_buf, &sidecar_path, enable_xmp_sync, &settings)
                };

                file_results.push(ImageFile {
                    path: virtual_path,
                    modified,
                    is_edited: metadata.is_edited,
                    tags: metadata.tags,
                    exif: None,
                    is_virtual_copy,
                    is_raw: metadata.is_raw,
                    group_id: None,
                    stack_id: None,
                    is_stack_leader: false,
                    rating: metadata.rating,
                    is_cloud_placeholder,
                });
            }

            file_results
        })
        .collect();

    assign_group_ids(&mut result_list, &settings);
    assign_stack_ids(&mut result_list);
    Ok(result_list)
}

#[tauri::command]
pub fn list_images_recursive(
    path: String,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    update_rotational_disk_flag(&path, &app_handle);

    let root_path = Path::new(&path);
    let mut images = Vec::new();

    let mut sidecars_by_path: HashMap<PathBuf, Vec<Option<String>>> = HashMap::new();

    for entry in WalkDir::new(root_path)
        .into_iter()
        .filter_entry(|e| {
            crate::library_ignore::should_visit(e.path(), e.depth(), e.file_type().is_dir())
        })
        .filter_map(Result::ok)
    {
        let entry_path = entry.path();
        if !entry_path.is_file() {
            continue;
        }

        let file_name = entry_path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(base) = file_name.strip_suffix(".rrdata") {
            let (source_filename, copy_id) =
                if base.len() >= 7 && base.as_bytes()[base.len() - 7] == b'.' {
                    let id = &base[base.len() - 6..];
                    if id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
                        (&base[..base.len() - 7], Some(id.to_string()))
                    } else {
                        (base, None)
                    }
                } else {
                    (base, None)
                };

            if let Some(parent) = entry_path.parent() {
                sidecars_by_path
                    .entry(parent.join(source_filename))
                    .or_default()
                    .push(copy_id);
            }
        } else if is_supported_image_file(entry_path.to_string_lossy().as_ref()) {
            images.push(entry_path.to_path_buf());
        }
    }

    let tasks: Vec<_> = images
        .into_iter()
        .map(|path_buf| {
            let sidecars = sidecars_by_path
                .remove(&path_buf)
                .unwrap_or_else(|| vec![None]);
            let path_str = path_buf.to_string_lossy().into_owned();
            let file_name = path_buf
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (path_str, file_name, path_buf, sidecars)
        })
        .collect();

    let mut result_list: Vec<ImageFile> = tasks
        .into_par_iter()
        .flat_map(|(path_str, file_name, path_buf, sidecars)| {
            let modified = fs::metadata(&path_buf)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_cloud_placeholder = is_cloud_placeholder(&path_buf);

            let mut file_results = Vec::with_capacity(sidecars.len());

            for copy_id_opt in sidecars {
                let (virtual_path, is_virtual_copy, sidecar_filename) = match copy_id_opt {
                    Some(id) => (
                        format!("{}?vc={}", path_str, id),
                        true,
                        format!("{}.{}.rrdata", file_name, id),
                    ),
                    None => (path_str.clone(), false, format!("{}.rrdata", file_name)),
                };

                let sidecar_path = path_buf.with_file_name(sidecar_filename);

                let xmp_is_placeholder = enable_xmp_sync
                    && resolve_xmp_path(&path_buf)
                        .is_some_and(|p| crate::file_management::is_cloud_placeholder(&p));

                let metadata = if crate::file_management::is_cloud_placeholder(&sidecar_path)
                    || xmp_is_placeholder
                {
                    enqueue_metadata(
                        &app_handle,
                        virtual_path.clone(),
                        path_buf.clone(),
                        sidecar_path.clone(),
                    );
                    ImageFileMetadata {
                        is_edited: false,
                        tags: None,
                        rating: 0,
                        is_raw: crate::formats::is_raw_file(&path_buf),
                    }
                } else {
                    resolve_image_metadata(&path_buf, &sidecar_path, enable_xmp_sync, &settings)
                };

                file_results.push(ImageFile {
                    path: virtual_path,
                    modified,
                    is_edited: metadata.is_edited,
                    tags: metadata.tags,
                    exif: None,
                    is_virtual_copy,
                    is_raw: metadata.is_raw,
                    group_id: None,
                    stack_id: None,
                    is_stack_leader: false,
                    rating: metadata.rating,
                    is_cloud_placeholder,
                });
            }

            file_results
        })
        .collect();

    assign_group_ids(&mut result_list, &settings);
    assign_stack_ids(&mut result_list);
    Ok(result_list)
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AlbumItem {
    Album {
        id: String,
        name: String,
        icon: Option<String>,
        images: Vec<String>,
    },
    Group {
        id: String,
        name: String,
        icon: Option<String>,
        children: Vec<AlbumItem>,
    },
}

fn get_albums_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    // BLITZRAW: one data directory, chosen and proved. See data_dir.rs.
    let albums_dir = crate::data_dir::data_path(app_handle, "albums");
    if !albums_dir.exists() {
        fs::create_dir_all(&albums_dir).map_err(|e| e.to_string())?;
    }
    Ok(albums_dir.join("albums.json"))
}

pub fn sort_album_tree(items: &mut [AlbumItem]) {
    items.sort_by(|a, b| {
        let get_sort_key = |item: &AlbumItem| match item {
            AlbumItem::Group { name, .. } => (0, name.to_lowercase()),
            AlbumItem::Album { name, .. } => (1, name.to_lowercase()),
        };

        let key_a = get_sort_key(a);
        let key_b = get_sort_key(b);

        key_a.cmp(&key_b)
    });

    for item in items.iter_mut() {
        if let AlbumItem::Group { children, .. } = item {
            sort_album_tree(children);
        }
    }
}

#[tauri::command]
pub fn get_albums(app_handle: AppHandle) -> Result<Vec<AlbumItem>, String> {
    let path = get_albums_path(&app_handle)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut items: Vec<AlbumItem> = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    sort_album_tree(&mut items);
    Ok(items)
}

#[tauri::command]
pub fn save_albums(mut tree: Vec<AlbumItem>, app_handle: AppHandle) -> Result<(), String> {
    let path = get_albums_path(&app_handle)?;
    sort_album_tree(&mut tree);
    let json_string = serde_json::to_string_pretty(&tree).map_err(|e| e.to_string())?;
    fs::write(path, json_string).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_to_album(
    album_id: String,
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let mut tree = get_albums(app_handle.clone())?;

    fn add_recursive(items: &mut [AlbumItem], target_id: &str, paths_to_add: &Vec<String>) -> bool {
        for item in items.iter_mut() {
            #[allow(clippy::collapsible_match)]
            match item {
                AlbumItem::Album { id, images, .. } if id == target_id => {
                    for p in paths_to_add {
                        if !images.contains(p) {
                            images.push(p.clone());
                        }
                    }
                    return true;
                }
                AlbumItem::Group { children, .. } => {
                    if add_recursive(children, target_id, paths_to_add) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    if add_recursive(&mut tree, &album_id, &paths) {
        save_albums(tree, app_handle)?;
    }
    Ok(())
}

fn sync_album_path_changes(
    app_handle: &AppHandle,
    renames: Option<&HashMap<String, String>>,
    deletions: Option<&HashSet<String>>,
    folder_rename: Option<(&str, &str)>,
) {
    if let Ok(mut tree) = get_albums(app_handle.clone()) {
        let mut changed = false;

        fn process_nodes(
            nodes: &mut [AlbumItem],
            renames: Option<&HashMap<String, String>>,
            deletions: Option<&HashSet<String>>,
            folder_rename: Option<(&str, &str)>,
            changed: &mut bool,
        ) {
            for node in nodes.iter_mut() {
                match node {
                    AlbumItem::Album { images, .. } => {
                        let mut new_images = Vec::new();

                        for img in images.drain(..) {
                            let mut current_img = img;

                            if let Some((old_folder, new_folder)) = folder_rename {
                                let img_path = Path::new(&current_img);
                                let old_path = Path::new(old_folder);
                                if let Ok(stripped) = img_path.strip_prefix(old_path) {
                                    let new_img_path = Path::new(new_folder).join(stripped);
                                    current_img = new_img_path.to_string_lossy().into_owned();
                                    *changed = true;
                                }
                            }

                            if let Some(r) = renames {
                                if let Some(new_path) = r.get(&current_img) {
                                    current_img = new_path.clone();
                                    *changed = true;
                                } else if let Some((base_path, vc_id)) =
                                    current_img.rsplit_once("?vc=")
                                    && let Some(new_base) = r.get(base_path)
                                {
                                    current_img = format!("{}?vc={}", new_base, vc_id);
                                    *changed = true;
                                }
                            }

                            let mut is_deleted = false;
                            if let Some(d) = deletions {
                                if d.contains(&current_img) {
                                    is_deleted = true;
                                } else {
                                    let img_path = Path::new(&current_img);
                                    for del_path_str in d {
                                        let del_path = Path::new(del_path_str);
                                        if img_path.starts_with(del_path) {
                                            is_deleted = true;
                                            break;
                                        }

                                        if let Some((base_path, _)) =
                                            current_img.rsplit_once("?vc=")
                                            && base_path == del_path_str
                                        {
                                            is_deleted = true;
                                            break;
                                        }
                                    }
                                }
                            }

                            if !is_deleted {
                                new_images.push(current_img);
                            } else {
                                *changed = true;
                            }
                        }
                        *images = new_images;
                    }
                    AlbumItem::Group { children, .. } => {
                        process_nodes(children, renames, deletions, folder_rename, changed);
                    }
                }
            }
        }

        process_nodes(&mut tree, renames, deletions, folder_rename, &mut changed);

        if changed {
            let _ = save_albums(tree, app_handle.clone());
        }
    }
}

#[tauri::command]
pub fn get_album_images(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<Vec<ImageFile>, String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    let mut result_list: Vec<ImageFile> = paths
        .into_par_iter()
        .filter_map(|virtual_path| {
            let (source_path, sidecar_path) = parse_virtual_path(&virtual_path);
            if !source_path.exists() {
                return None;
            }

            let modified = fs::metadata(&source_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);

            let is_virtual_copy = virtual_path.contains("?vc=");
            let is_cloud_placeholder = is_cloud_placeholder(&source_path);

            let xmp_is_placeholder = enable_xmp_sync
                && resolve_xmp_path(&source_path)
                    .is_some_and(|p| crate::file_management::is_cloud_placeholder(&p));

            let metadata = if crate::file_management::is_cloud_placeholder(&sidecar_path)
                || xmp_is_placeholder
            {
                enqueue_metadata(
                    &app_handle,
                    virtual_path.clone(),
                    source_path.clone(),
                    sidecar_path.clone(),
                );
                ImageFileMetadata {
                    is_edited: false,
                    tags: None,
                    rating: 0,
                    is_raw: crate::formats::is_raw_file(&source_path),
                }
            } else {
                resolve_image_metadata(&source_path, &sidecar_path, enable_xmp_sync, &settings)
            };

            Some(ImageFile {
                path: virtual_path.clone(),
                modified,
                is_edited: metadata.is_edited,
                tags: metadata.tags,
                exif: None,
                is_virtual_copy,
                is_raw: metadata.is_raw,
                group_id: None,
                stack_id: None,
                is_stack_leader: false,
                rating: metadata.rating,
                is_cloud_placeholder,
            })
        })
        .collect();

    assign_group_ids(&mut result_list, &settings);
    assign_stack_ids(&mut result_list);
    Ok(result_list)
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FolderNode {
    pub name: String,
    pub path: String,
    pub children: Vec<FolderNode>,
    pub is_dir: bool,
    pub image_count: usize,
    pub has_subdirs: bool,
    pub modified: u64,
    pub created: u64,
}

fn has_subdirs(path: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.filter_map(Result::ok) {
            if let Ok(file_type) = entry.file_type()
                && file_type.is_dir()
            {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with('.') {
                    return true;
                }
            }
        }
    }
    false
}

fn scan_dir_lazy(
    path: &Path,
    expanded_folders: &HashSet<&str>,
    show_image_counts: bool,
    prefetch_one_level: bool,
) -> Result<(Vec<FolderNode>, usize), std::io::Error> {
    let mut children_folders = Vec::new();
    let mut current_dir_image_count = 0;

    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Could not scan directory '{}': {}", path.display(), e);
            return Ok((Vec::new(), 0));
        }
    };

    for entry in entries.filter_map(Result::ok) {
        let current_path = entry.path();

        // Keep catalog preview trees and system folders out of the tree; they
        // hold thousands of derivative files and no photographs.
        if entry
            .file_name()
            .to_str()
            .map(crate::library_ignore::is_ignored_directory_name)
            .unwrap_or(false)
            && current_path.is_dir()
        {
            continue;
        }

        let (file_type, modified, created) = match entry.metadata() {
            Ok(meta) => {
                let ft = meta.file_type();
                let mod_time = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                let cre_time = meta.created().unwrap_or(mod_time);

                (
                    ft,
                    mod_time
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    cre_time
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                )
            }
            Err(_) => continue,
        };

        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if name_str.starts_with('.') {
            continue;
        }

        if file_type.is_dir() {
            let path_str = current_path.to_string_lossy().into_owned();
            let is_expanded = expanded_folders.contains(path_str.as_str());

            let should_scan = is_expanded || prefetch_one_level;
            let next_prefetch = is_expanded;

            let (grand_children, sub_dir_own_images) = if should_scan {
                scan_dir_lazy(
                    &current_path,
                    expanded_folders,
                    show_image_counts,
                    next_prefetch,
                )?
            } else {
                let count = if show_image_counts {
                    WalkDir::new(&current_path)
                        .into_iter()
                        .filter_map(Result::ok)
                        .filter(|e| {
                            e.file_type().is_file()
                                && crate::formats::is_supported_image_file(e.path())
                        })
                        .count()
                } else {
                    0
                };
                (Vec::new(), count)
            };

            let has_any_subdirs = if should_scan {
                grand_children.iter().any(|c| c.is_dir)
            } else {
                has_subdirs(&current_path)
            };

            let grand_children_sum: usize = grand_children.iter().map(|c| c.image_count).sum();
            let total_child_count = sub_dir_own_images + grand_children_sum;

            children_folders.push(FolderNode {
                name: name_str.into_owned(),
                path: path_str,
                children: grand_children,
                is_dir: true,
                image_count: total_child_count,
                has_subdirs: has_any_subdirs,
                modified,
                created,
            });
        } else if show_image_counts
            && file_type.is_file()
            && crate::formats::is_supported_image_file(&current_path)
        {
            current_dir_image_count += 1;
        }
    }

    children_folders.sort_by_key(|a| a.name.to_lowercase());

    Ok((children_folders, current_dir_image_count))
}

fn get_folder_tree_sync(
    path: String,
    expanded_folders: Vec<String>,
    show_image_counts: bool,
) -> Result<FolderNode, String> {
    let root_path = Path::new(&path);
    if !root_path.is_dir() {
        return Err(format!("Directory does not exist: {}", path));
    }

    let (modified, created) = root_path
        .metadata()
        .map(|m| {
            let mod_time = m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let cre_time = m.created().unwrap_or(mod_time);
            (
                mod_time
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                cre_time
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            )
        })
        .unwrap_or((0, 0));

    let expanded_set: HashSet<&str> = expanded_folders.iter().map(|s| s.as_str()).collect();

    let (children, own_count) = scan_dir_lazy(root_path, &expanded_set, show_image_counts, true)
        .map_err(|e| e.to_string())?;

    let children_sum: usize = children.iter().map(|c| c.image_count).sum();
    let has_subdirs = children.iter().any(|c| c.is_dir);

    let name = match root_path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => {
            let trimmed = path.trim_end_matches(&['/', '\\'][..]);
            if trimmed.is_empty() {
                path.clone()
            } else {
                trimmed.to_string()
            }
        }
    };

    Ok(FolderNode {
        name,
        path: path.clone(),
        children,
        is_dir: true,
        image_count: own_count + children_sum,
        has_subdirs,
        modified,
        created,
    })
}

#[tauri::command]
pub async fn get_folder_children(
    path: String,
    show_image_counts: bool,
) -> Result<Vec<FolderNode>, String> {
    match tauri::async_runtime::spawn_blocking(move || {
        let root_path = Path::new(&path);
        if !root_path.is_dir() {
            return Err(format!("Directory does not exist: {}", path));
        }
        let empty_set = HashSet::new();
        let (children, _) = scan_dir_lazy(root_path, &empty_set, show_image_counts, false)
            .map_err(|e| e.to_string())?;

        Ok(children)
    })
    .await
    {
        Ok(Ok(children)) => Ok(children),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("Task failed: {}", e)),
    }
}

#[tauri::command]
pub async fn get_folder_tree(
    path: String,
    expanded_folders: Vec<String>,
    show_image_counts: bool,
) -> Result<FolderNode, String> {
    match tauri::async_runtime::spawn_blocking(move || {
        get_folder_tree_sync(path, expanded_folders, show_image_counts)
    })
    .await
    {
        Ok(Ok(folder_node)) => Ok(folder_node),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(format!("Failed to execute folder tree task: {}", e)),
    }
}

#[tauri::command]
pub async fn get_pinned_folder_trees(
    paths: Vec<String>,
    expanded_folders: Vec<String>,
    show_image_counts: bool,
) -> Result<Vec<FolderNode>, String> {
    let result = tauri::async_runtime::spawn_blocking(move || {
        let results: Vec<Result<FolderNode, String>> = paths
            .par_iter()
            .map(|path| {
                get_folder_tree_sync(path.clone(), expanded_folders.clone(), show_image_counts)
            })
            .collect();

        let mut folder_nodes = Vec::new();
        for result in results {
            match result {
                Ok(node) => folder_nodes.push(node),
                Err(e) => log::warn!("Failed to get tree for pinned folder: {}", e),
            }
        }
        folder_nodes
    })
    .await;

    match result {
        Ok(nodes) => Ok(nodes),
        Err(e) => Err(format!("Task failed: {}", e)),
    }
}

/// Checks if the given path exists and is an iCloud placeholder file on macOS.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    const SF_DATALESS: u32 = 0x4000_0000;

    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::lstat(c_path.as_ptr(), &mut stat_buf) };
    ret == 0 && (stat_buf.st_flags & SF_DATALESS) != 0
}

#[cfg(not(target_os = "macos"))]
pub fn is_cloud_placeholder(_path: &Path) -> bool {
    false
}

pub fn read_file_mapped(path: &Path) -> Result<Mmap, ReadFileError> {
    if !path.is_file() {
        return Err(ReadFileError::Invalid);
    }
    if !path.exists() {
        return Err(ReadFileError::NotFound);
    }
    if path.metadata().map_err(ReadFileError::Io)?.len() == 0 {
        return Err(ReadFileError::Empty);
    }
    let file = fs::File::open(path).map_err(ReadFileError::Io)?;
    if file.try_lock_shared().is_err() {
        return Err(ReadFileError::Locked);
    }
    let mmap = unsafe {
        MmapOptions::new()
            .len(file.metadata().map_err(ReadFileError::Io)?.len() as usize)
            .map(&file)
            .map_err(ReadFileError::Io)?
    };
    Ok(mmap)
}

fn find_embedded_jpeg(exif: &exif::Exif, ifd: exif::In) -> Option<&[u8]> {
    let offset = exif
        .get_field(exif::Tag::JPEGInterchangeFormat, ifd)?
        .value
        .get_uint(0)? as usize;
    let len = exif
        .get_field(exif::Tag::JPEGInterchangeFormatLength, ifd)?
        .value
        .get_uint(0)? as usize;
    exif.buf().get(offset..offset + len)
}

fn apply_exif_orientation(img: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

fn try_load_embedded_raw_preview(source_path: &Path, target_res: u32) -> Option<DynamicImage> {
    let mmap = read_file_mapped(source_path).ok()?;
    let exif = exif_processing::read_exif(&mmap)?;

    let (jpeg_bytes, ifd) = find_embedded_jpeg(&exif, exif::In::PRIMARY)
        .map(|b| (b, exif::In::PRIMARY))
        .or_else(|| {
            find_embedded_jpeg(&exif, exif::In::THUMBNAIL).map(|b| (b, exif::In::THUMBNAIL))
        })?;

    let img = image::load_from_memory_with_format(jpeg_bytes, image::ImageFormat::Jpeg).ok()?;

    if img.width().max(img.height()) < (target_res as f32 * 0.95) as u32 {
        return None;
    }

    let orientation = exif
        .get_field(exif::Tag::Orientation, ifd)
        .and_then(|f| f.value.get_uint(0))
        .unwrap_or(1);

    Some(apply_exif_orientation(img, orientation))
}

pub fn generate_thumbnail_data(
    path_str: &str,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    app_handle: &AppHandle,
    // BLITZRAW: the width to render at, when the caller is not a thumbnail.
    // The preview cache asks for the editor's preview resolution and would
    // otherwise be handed a 720 pixel picture to enlarge.
    target_res_override: Option<u32>,
) -> anyhow::Result<DynamicImage> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);
    let source_path_str = source_path.to_string_lossy().to_string();
    let is_raw = is_raw_file(&source_path_str);

    let metadata: Option<ImageMetadata> = if is_cloud_placeholder(&sidecar_path) {
        enqueue_metadata(
            app_handle,
            path_str.to_string(),
            source_path.clone(),
            sidecar_path.clone(),
        );
        None
    } else {
        fs::read_to_string(&sidecar_path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
    };

    let adjustments = metadata
        .as_ref()
        .map_or(serde_json::Value::Null, |m| m.adjustments.clone());

    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let always_decode_raw = settings.always_decode_raw_thumbnails.unwrap_or(false);

    if is_raw
        && adjustments.is_null()
        && preloaded_image.is_none()
        && !always_decode_raw
        && target_res_override.is_none()
    {
        let target_res = settings.thumbnail_resolution.unwrap_or(720);
        if let Some(preview) = try_load_embedded_raw_preview(&source_path, target_res) {
            return Ok(preview);
        }
    }

    if let (Some(context), Some(meta)) = (gpu_context, metadata)
        && !meta.adjustments.is_null()
    {
        let state = app_handle.state::<AppState>();
        let target_res =
            target_res_override.unwrap_or_else(|| settings.thumbnail_resolution.unwrap_or(720));

        let base_cache_hash = crate::cache_utils::calculate_thumbnail_base_hash(&meta.adjustments);
        // BLITZRAW: the base is a downscale to the target, so a 720 one cannot
        // stand in for a 1920 one. Keyed by width as well as path, or a preview
        // built after a thumbnail would quietly come out at thumbnail size.
        let geometry_cache_key = format!("{path_str}@{target_res}");

        let crop_data: Option<Crop> = serde_json::from_value(meta.adjustments["crop"].clone()).ok();

        let cached_base: Option<(DynamicImage, f32)> = {
            let cache = state.thumbnail_geometry_cache.lock().unwrap();
            if let Some((cached_hash, img, scale)) = cache.get(&geometry_cache_key) {
                let mut sufficient_resolution = true;
                if let Some(c) = &crop_data
                    && c.width > 0.0
                    && c.height > 0.0
                {
                    let final_crop_max_dim =
                        (c.width as f32 * *scale).max(c.height as f32 * *scale);
                    if final_crop_max_dim < (target_res as f32 * 0.95) {
                        sufficient_resolution = false;
                    }
                }

                if *cached_hash == base_cache_hash && sufficient_resolution {
                    Some((img.clone(), *scale))
                } else {
                    None
                }
            } else {
                None
            }
        };

        let (processing_base, total_scale) = if let Some(hit) = cached_base {
            hit
        } else {
            let mut raw_scale_factor = 1.0f32;

            let composite_image = if let Some(img) = preloaded_image {
                // Already the full decode, so a patch's recorded coordinates
                // are this image's coordinates.
                image_loader::composite_patches_on_image(img, &adjustments)?
            } else {
                let mmap_guard;
                let vec_guard;

                let file_slice: &[u8] = match read_file_mapped(&source_path) {
                    Ok(mmap) => {
                        mmap_guard = Some(mmap);
                        mmap_guard.as_ref().unwrap()
                    }
                    Err(e) => {
                        if preloaded_image.is_none() {
                            log::warn!("Fallback read for {}: {}", source_path_str, e);
                        }
                        let bytes = fs::read(&source_path).map_err(|io_err| {
                            anyhow::anyhow!(
                                "Fallback read failed for {}: {}",
                                source_path_str,
                                io_err
                            )
                        })?;
                        vec_guard = Some(bytes);
                        vec_guard.as_ref().unwrap()
                    }
                };

                // BLITZRAW: loaded and composited as two steps, because the
                // scale of the develop has to be known before the patches go
                // on. `load_and_composite` did both at once, so a half-size
                // fast develop got its healing patches pasted at full-size
                // coordinates: enlarged, and displaced towards the middle. The
                // scale is only knowable here, from the decoded size against
                // the size the file says it is.
                let img = image_loader::load_base_image_from_bytes(
                    file_slice,
                    &source_path_str,
                    true,
                    &settings,
                    None,
                )?;

                if is_raw {
                    raw_scale_factor = crate::raw_processing::get_fast_demosaic_scale_factor(
                        file_slice,
                        img.width(),
                        img.height(),
                    );
                }

                image_loader::composite_patches_on_image_scaled(
                    &img,
                    &adjustments,
                    raw_scale_factor,
                )?
            };

            let warped_image =
                apply_geometry_warp(Cow::Borrowed(&composite_image), &meta.adjustments);

            let blurred_image = crate::lens_blur::apply_lens_blur(warped_image, &meta.adjustments);

            let orientation_steps =
                meta.adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
            let coarse_rotated_image = apply_coarse_rotation(blurred_image, orientation_steps);

            let (full_w, full_h) = coarse_rotated_image.dimensions();

            let mut processing_dim = target_res;
            if let Some(c) = &crop_data
                && c.width > 0.0
                && c.height > 0.0
            {
                let crop_max_dim_loaded = c.width.max(c.height) * raw_scale_factor as f64;
                let full_max_dim = full_w.max(full_h) as f64;
                if crop_max_dim_loaded > 0.0 {
                    processing_dim = ((target_res as f64 * full_max_dim / crop_max_dim_loaded)
                        .round() as u32)
                        .min(full_w.max(full_h));
                }
            }

            let (base, gpu_scale) = if full_w > processing_dim || full_h > processing_dim {
                let base = crate::image_processing::downscale_f32_image(
                    &coarse_rotated_image,
                    processing_dim,
                    processing_dim,
                );
                let scale = if full_w > 0 {
                    base.width() as f32 / full_w as f32
                } else {
                    1.0
                };
                (base, scale)
            } else {
                (coarse_rotated_image.into_owned(), 1.0)
            };

            let total_scale = gpu_scale * raw_scale_factor;

            let mut cache = state.thumbnail_geometry_cache.lock().unwrap();
            // BLITZRAW: one goes, not all of them.
            //
            // Each entry is a downscaled float image, tens of megabytes. Wiping
            // the lot on reaching the limit meant that a selection of
            // twenty-eight photos sat right on the edge of it: one more entry
            // and every one of those photos had to be decoded from the raw
            // again. That is the sawtooth between twelve and twenty gigabytes,
            // and it repeated on every press.
            //
            // Which one to drop is arbitrary, because nothing here records when
            // an entry was last wanted. Arbitrary and single is still strictly
            // better than all of them: the photo being worked on stays cached
            // most of the time instead of never.
            while cache.len() >= GEOMETRY_CACHE_LIMIT
                && let Some(victim) = cache
                    .keys()
                    .find(|held| *held != &geometry_cache_key)
                    .cloned()
            {
                cache.remove(&victim);
            }
            cache.insert(
                geometry_cache_key.clone(),
                (base_cache_hash, base.clone(), total_scale),
            );

            (base, total_scale)
        };

        let rotation_degrees = meta.adjustments["rotation"].as_f64().unwrap_or(0.0) as f32;
        let flip_horizontal = meta.adjustments["flipHorizontal"]
            .as_bool()
            .unwrap_or(false);
        let flip_vertical = meta.adjustments["flipVertical"].as_bool().unwrap_or(false);

        let flipped_image = apply_flip(Cow::Owned(processing_base), flip_horizontal, flip_vertical);
        let rotated_image = apply_rotation(flipped_image, rotation_degrees);

        let scaled_crop_json = if let Some(c) = &crop_data {
            serde_json::to_value(Crop {
                x: c.x * total_scale as f64,
                y: c.y * total_scale as f64,
                width: c.width * total_scale as f64,
                height: c.height * total_scale as f64,
            })
            .unwrap_or(serde_json::Value::Null)
        } else {
            serde_json::Value::Null
        };

        let cropped_preview = apply_crop(rotated_image, &scaled_crop_json);
        let (preview_w, preview_h) = cropped_preview.dimensions();
        let unscaled_crop_offset = crop_data.map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));

        let mask_definitions: Vec<MaskDefinition> = meta
            .adjustments
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_else(Vec::new);

        let mask_bitmaps: Vec<ImageBuffer<Luma<u8>, Vec<u8>>> = mask_definitions
            .iter()
            .filter_map(|def| {
                crate::get_cached_or_generate_mask(
                    &state,
                    def,
                    preview_w,
                    preview_h,
                    total_scale,
                    (
                        unscaled_crop_offset.0 * total_scale,
                        unscaled_crop_offset.1 * total_scale,
                    ),
                    &meta.adjustments,
                )
            })
            .collect();

        let tm_override = crate::image_processing::resolve_tonemapper_override(&settings, is_raw);
        let mut gpu_adjustments =
            get_all_adjustments_from_json(&meta.adjustments, is_raw, tm_override);
        // BLITZRAW: white balance from the camera's own calibration.
        crate::image_processing::apply_camera_profile_to_adjustments(
            &mut gpu_adjustments,
            path_str,
            &meta.adjustments,
        );
        let lut_path = meta.adjustments["lutPath"].as_str();
        let lut = lut_path.and_then(|p| {
            let mut cache = state.lut_cache.lock().unwrap();
            if let Some(cached_lut) = cache.get(p) {
                return Some(cached_lut.clone());
            }
            if let Ok(loaded_lut) = crate::lut_processing::parse_lut_file(p) {
                let arc_lut = Arc::new(loaded_lut);
                cache.insert(p.to_string(), arc_lut.clone());
                return Some(arc_lut);
            }
            None
        });

        let mut hasher = DefaultHasher::new();
        path_str.hash(&mut hasher);
        meta.adjustments.to_string().hash(&mut hasher);
        let unique_hash = hasher.finish();

        if let Ok(processed_image) = gpu_processing::process_and_get_dynamic_image(
            context,
            &state,
            cropped_preview.as_ref(),
            unique_hash,
            gpu_processing::RenderRequest {
                adjustments: gpu_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "generate_thumbnail_data",
        ) {
            return Ok(processed_image);
        } else {
            return Ok(cropped_preview.into_owned());
        }
    }

    let mut final_image = if let Some(img) = preloaded_image {
        image_loader::composite_patches_on_image(img, &adjustments)?
    } else {
        match read_file_mapped(&source_path) {
            Ok(mmap) => image_loader::load_and_composite(
                &mmap,
                &source_path_str,
                &adjustments,
                true,
                &settings,
                None,
            )?,
            Err(e) => {
                log::warn!("Fallback read for {}: {}", source_path_str, e);
                let bytes = fs::read(&source_path)?;
                image_loader::load_and_composite(
                    &bytes,
                    &source_path_str,
                    &adjustments,
                    true,
                    &settings,
                    None,
                )?
            }
        }
    };

    if adjustments.is_null() {
        let default_tm = if is_raw {
            settings.default_raw_tonemapper.as_deref().unwrap_or("agx")
        } else {
            settings
                .default_non_raw_tonemapper
                .as_deref()
                .unwrap_or("basic")
        };
        if default_tm == "agx" {
            if !is_raw {
                final_image = crate::image_processing::apply_srgb_to_linear(final_image);
            }
            crate::image_processing::apply_cpu_agx_tonemap(&mut final_image);
        } else if is_raw {
            apply_cpu_default_raw_processing(&mut final_image);
        }
    }

    let fallback_orientation_steps = adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
    Ok(apply_coarse_rotation(Cow::Owned(final_image), fallback_orientation_steps).into_owned())
}

pub(crate) fn encode_thumbnail(image: &DynamicImage, target_width: u32) -> Result<Vec<u8>> {
    let thumbnail = crate::image_processing::downscale_f32_image(image, target_width, target_width);
    let mut buf = Cursor::new(Vec::new());
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 75);
    encoder.encode_image(&thumbnail.to_rgb8())?;
    Ok(buf.into_inner())
}

// ===================== BLITZRAW: in-camera ratings =====================
/// The rating a `.xmp` sidecar or the photo's own embedded packet carries, for
/// the paths that read a `.rrdata` sidecar directly instead of going through
/// `resolve_image_metadata`. Returns 0 when there is no opinion to be had.
fn external_rating_for(source_path: &Path, settings: &AppSettings) -> u8 {
    if !settings.enable_xmp_sync.unwrap_or(false) {
        return 0;
    }

    let mut metadata = ImageMetadata::default();
    sync_metadata_from_xmp(source_path, &mut metadata);
    sync_metadata_from_embedded_xmp(source_path, &mut metadata);
    metadata.rating
}

/// Whether this photo still has to be asked what the camera said. False once
/// the sidecar records an answer, which is the usual case after one scan.
fn needs_camera_rating(metadata: &ImageMetadata) -> bool {
    metadata.rating == 0 && metadata.camera_rating.is_none()
}
// =================== BLITZRAW END: in-camera ratings ===================

fn generate_single_thumbnail_and_cache(
    path_str: &str,
    thumb_cache_dir: &Path,
    gpu_context: Option<&GpuContext>,
    preloaded_image: Option<&DynamicImage>,
    force_regenerate: bool,
    app_handle: &AppHandle,
    settings: &AppSettings,
) -> Option<(String, u8, bool)> {
    let (source_path, sidecar_path) = parse_virtual_path(path_str);

    let (rating, is_edited, adjustments) = if is_cloud_placeholder(&sidecar_path) {
        enqueue_metadata(
            app_handle,
            path_str.to_string(),
            source_path.clone(),
            sidecar_path.clone(),
        );
        (0, false, Value::Null)
    } else if let Ok(content) = fs::read_to_string(&sidecar_path) {
        if let Ok(meta) = serde_json::from_str::<ImageMetadata>(&content) {
            let is_raw = crate::formats::is_raw_file(path_str);
            let tm = crate::image_processing::resolve_tonemapper_override(settings, is_raw);

            // BLITZRAW: until the scan has recorded an answer, a sidecar reading
            // zero is not yet a rating. Emitting it as one overwrote the stars
            // the grid had just been given.
            let rating = if needs_camera_rating(&meta) {
                external_rating_for(&source_path, settings)
            } else {
                meta.rating
            };

            (
                rating,
                crate::image_processing::is_image_edited(&meta.adjustments, is_raw, tm),
                meta.adjustments,
            )
        } else {
            (0, false, Value::Null)
        }
    } else {
        // BLITZRAW: no sidecar, so the camera's own stars are the only rating
        // this photo has. Without this the grid got the rating from
        // `resolve_image_metadata` and then had it overwritten with zero by the
        // thumbnail that followed.
        (
            external_rating_for(&source_path, settings),
            false,
            Value::Null,
        )
    };

    let cache_path = thumbnail_path_for(&thumb_cache_dir, path_str);

    if !force_regenerate
        && cache_path.exists()
        && thumbnail_is_current(&cache_path, path_str, &adjustments, settings)
    {
        return Some((cache_path.to_string_lossy().into_owned(), rating, is_edited));
    }

    if is_cloud_placeholder(&source_path) {
        return None;
    }

    let target_width = settings.thumbnail_resolution.unwrap_or(720);

    // BLITZRAW: a photo that already has a cached preview is rendered at the
    // preview width instead, and its thumbnail is taken from that same picture,
    // since a thumbnail is a downscale of whatever it is handed. The decode is
    // what costs, so this keeps previews current through every path that
    // regenerates a thumbnail for about thirty milliseconds on top of a second
    // and a half, rather than rendering the photo twice. None for a photo with
    // no preview, which is most of them, and those are untouched.
    let preview_width = crate::preview_cache::refresh_width_for(path_str, app_handle);

    if let Ok(thumb_image) = generate_thumbnail_data(
        path_str,
        gpu_context,
        preloaded_image,
        app_handle,
        preview_width,
    ) {
        // Before the thumbnail, so a folder that will not take a preview still
        // gets one of those, and is complained about rather than failed on.
        if let Some(width) = preview_width
            && let Err(e) =
                crate::preview_cache::store_rendered_preview(path_str, &thumb_image, width)
        {
            log::warn!("Could not refresh the preview for {path_str}: {e}");
        }

        if let Ok(thumb_data) = encode_thumbnail(&thumb_image, target_width) {
            match write_thumbnail(&cache_path, &thumb_data) {
                Ok(()) => {
                    // BLITZRAW: what this picture was made from, recorded beside
                    // it, so nothing has to guess later from a file's date.
                    // Written after the picture: a key without its picture would
                    // claim a thumbnail that is not there, and a picture without
                    // its key is only judged the old way once.
                    if let Some(key) = thumbnail_freshness_key(path_str, &adjustments, settings) {
                        let _ = fs::write(thumbnail_key_path(&cache_path), key);
                    }
                }
                Err(e) => {
                    log::warn!("Could not write the thumbnail for {path_str}: {e}");
                    return None;
                }
            }
            return Some((cache_path.to_string_lossy().into_owned(), rating, is_edited));
        }
    }
    None
}

/// How many downscaled develop bases are kept in memory at once.
///
/// Each is a float image at preview width, so about thirty megabytes for a
/// full-frame photo. Twelve is roughly a third of a gigabyte, which is the
/// most this is worth spending to save decodes.
const GEOMETRY_CACHE_LIMIT: usize = 12;

#[cfg(test)]
mod freshness_tests {
    use super::*;

    fn photo(name: &str) -> String {
        let dir = std::env::temp_dir().join("blitzraw-freshness-tests");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join(name);
        fs::write(&path, b"not really a raw, but it has a size and a date").unwrap();
        path.to_string_lossy().into_owned()
    }

    fn key(path: &str, adjustments: &Value, settings: &AppSettings) -> String {
        thumbnail_freshness_key(path, adjustments, settings).expect("the photo is on disk")
    }

    /// The whole point. A star, a colour label, a tag and a note about the
    /// camera's own rating all live in the same file as the adjustments, and
    /// the old rule discarded every cached picture whenever any of them was
    /// written. A folder scan writes a lot of them.
    #[test]
    fn what_does_not_change_the_picture_does_not_change_the_key() {
        let path = photo("stars.nef");
        let settings = AppSettings::default();
        let adjustments = serde_json::json!({ "exposure": 0.5 });

        let before = key(&path, &adjustments, &settings);
        // The rating, tags and camera_rating are not passed in at all, which is
        // the point: they cannot reach this. Same adjustments, same key.
        let after = key(&path, &adjustments, &settings);
        assert_eq!(before, after);
    }

    #[test]
    fn an_adjustment_changes_the_key() {
        let path = photo("nudged.nef");
        let settings = AppSettings::default();
        let before = key(&path, &serde_json::json!({ "exposure": 0.5 }), &settings);
        let after = key(&path, &serde_json::json!({ "exposure": 0.6 }), &settings);
        assert_ne!(before, after, "a tenth of a stop is a different picture");
    }

    #[test]
    fn the_photo_being_rewritten_changes_the_key() {
        let path = photo("remerged.dng");
        let settings = AppSettings::default();
        let adjustments = serde_json::json!({});
        let before = key(&path, &adjustments, &settings);

        // A merge written a second time over its own name. Same path, same
        // sidecar, different picture: this is the case where the thumbnail
        // refused to update.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(
            &path,
            b"a different set of pixels entirely, and a different length",
        )
        .unwrap();

        assert_ne!(before, key(&path, &adjustments, &settings));
    }

    #[test]
    fn a_setting_that_changes_the_render_changes_the_key() {
        let path = photo("resized.nef");
        let adjustments = serde_json::json!({ "exposure": 0.0 });

        let mut small = AppSettings::default();
        small.thumbnail_resolution = Some(720);
        let mut large = AppSettings::default();
        large.thumbnail_resolution = Some(1440);

        assert_ne!(
            key(&path, &adjustments, &small),
            key(&path, &adjustments, &large)
        );
    }

    /// A setting that does not touch the picture must not throw the cache away.
    #[test]
    fn a_setting_that_does_not_touch_the_picture_leaves_the_key_alone() {
        let path = photo("unrelated.nef");
        let adjustments = serde_json::json!({ "exposure": 0.0 });

        let mut before = AppSettings::default();
        before.thumbnail_worker_threads = Some(4);
        let mut after = AppSettings::default();
        after.thumbnail_worker_threads = Some(8);

        assert_eq!(
            key(&path, &adjustments, &before),
            key(&path, &adjustments, &after)
        );
    }
}

// ======== BLITZRAW: a small picture that is provably of this photo ========
/// The cached thumbnail, only if it is the picture these adjustments produce.
///
/// `scopes_from_small_picture` takes whatever thumbnail is there, which is right
/// for scopes: a stale one is off by a nudge and still tells you which frame is
/// hotter. It is wrong for the proxy preview, because a nudge is applied as a
/// difference from a known base, and a difference from an unknown base is
/// worse than showing nothing.
pub fn current_thumbnail_for(path_str: &str, app_handle: &AppHandle) -> Option<PathBuf> {
    let dir = get_thumb_cache_dir(app_handle).ok()?;
    let cache_path = thumbnail_path_for(&dir, path_str);
    if !cache_path.exists() {
        return None;
    }
    let (_, sidecar) = parse_virtual_path(path_str);
    let adjustments = crate::exif_processing::load_sidecar(&sidecar).adjustments;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    thumbnail_is_current(&cache_path, path_str, &adjustments, &settings).then_some(cache_path)
}
// ====== BLITZRAW END: a small picture that is provably of this photo ======

fn prefetch_source_file(path_str: &str) {
    let (source_path, _) = parse_virtual_path(path_str);
    let _ = fs::read(&source_path);
}

// ============ BLITZRAW: one queue owns every rebuild ============
/// How quiet the marking has to go before a changed photo is rendered.
///
/// A run of key presses marks the same photos every few tens of milliseconds.
/// Rendering on each of them is what turned ten presses across twenty-eight
/// photos into two hundred and eighty raw decodes, several gigabytes of float
/// images, and a window frozen for a couple of minutes.
const REBUILD_SETTLE: Duration = Duration::from_millis(220);

/// And how long a photo may be pushed back by more marking before it is
/// rendered anyway, so a held key still shows something happening.
const REBUILD_MAX_WAIT: Duration = Duration::from_millis(1500);

/// How often an idle worker looks again, since a settle runs out with nothing
/// to wake it.
const REBUILD_POLL: Duration = Duration::from_millis(60);

/// What a worker has claimed, and whether it must render rather than trust the
/// file already in the cache.
struct ThumbnailJob {
    path: String,
    force: bool,
}

/// Says the picture of these photos has changed, and renders nothing.
///
/// # Why nothing renders here
///
/// Every command that changed a photo used to render its thumbnails itself,
/// there and then, across every core. Ten commands in the air at once meant ten
/// of those overlapping, each re-decoding the same files, each holding a
/// full-size float image per core. Nothing merged them, because nothing knew
/// about anything else.
///
/// They all say the same sentence to the same place now, and the queue decides
/// when. Ten presses across twenty-eight photos is twenty-eight renders once
/// the pressing stops, on the four worker threads that already existed, rather
/// than two hundred and eighty at once.
///
/// The one exception is a caller that already holds the decoded pixels: a merge
/// just made, an auto-adjust that has just analysed the photo. Throwing that
/// decode away to do it again later is worse than rendering on the spot, so
/// those render and announce it themselves.
pub fn mark_pictures_changed(app_handle: &AppHandle, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let state = app_handle.state::<crate::AppState>();
    let now = Instant::now();
    let mut newly_dirty = 0usize;
    {
        let mut dirty = state.thumbnail_manager.dirty.lock().unwrap();
        for path in paths {
            match dirty.get_mut(path) {
                // Already waiting. Push its settle back rather than adding a
                // second job for the same photo.
                Some(entry) => entry.last_marked = now,
                None => {
                    dirty.insert(
                        path.clone(),
                        crate::app_state::DirtyPicture {
                            first_marked: now,
                            last_marked: now,
                        },
                    );
                    newly_dirty += 1;
                }
            }
        }
    }
    if newly_dirty > 0 {
        add_to_thumbnail_queue(&state, newly_dirty, app_handle);
    }
    state.thumbnail_manager.cvar.notify_all();
}

/// Whether a changed photo has waited long enough to be worth rendering.
///
/// Quiet for a moment, or dirty for long enough that waiting for quiet is no
/// longer honest. A finger held on a key is the second case.
fn is_ready_to_render(waiting: &crate::app_state::DirtyPicture, now: Instant) -> bool {
    now.duration_since(waiting.last_marked) >= REBUILD_SETTLE
        || now.duration_since(waiting.first_marked) >= REBUILD_MAX_WAIT
}

/// Takes the next piece of work, changed photos before scrolled-to ones.
///
/// Returns nothing while a changed photo is only waiting out its settle, which
/// is why the caller waits with a timeout rather than for a notification.
///
/// Also returns how many queued paths were dropped for already being rendered,
/// which the caller counts against the progress bar. Reported rather than
/// counted here so that this makes its decision without needing a running
/// application, and can therefore be tested.
fn claim_next_job(
    manager: &crate::app_state::ThumbnailManager,
    now: Instant,
) -> (Option<ThumbnailJob>, usize) {
    // A change the user just made comes first. It is also the only work here
    // that must not be dropped: scrolling rewrites the other lane.
    {
        let mut dirty = manager.dirty.lock().unwrap();
        if !dirty.is_empty() {
            let ready = dirty
                .iter()
                .filter(|(_, waiting)| is_ready_to_render(waiting, now))
                .min_by_key(|(_, waiting)| waiting.first_marked)
                .map(|(path, _)| path.clone());

            if let Some(path) = ready {
                let mut processing = manager.processing_now.lock().unwrap();
                // Being rendered right now by another worker. Left dirty, so it
                // is rendered again after that finishes and picks up whatever
                // changed while it was working.
                if !processing.contains(&path) {
                    dirty.remove(&path);
                    processing.insert(path.clone());
                    return (Some(ThumbnailJob { path, force: true }), 0);
                }
            }
        }
    }

    // Then whatever the grid has scrolled into view.
    let mut skipped = 0usize;
    let claimed = {
        let mut queue = manager.queue.lock().unwrap();
        let mut claimed = None;
        while let Some(path) = queue.pop_back() {
            let mut processing = manager.processing_now.lock().unwrap();
            if processing.contains(&path) {
                skipped += 1;
                continue;
            }
            processing.insert(path.clone());
            claimed = Some(path);
            break;
        }
        claimed
    };

    (
        claimed.map(|path| ThumbnailJob { path, force: false }),
        skipped,
    )
}

/// The full decode of the open photo, when this job is that photo.
///
/// Rendering from it skips a second and a half of decoding. Only ever the one
/// photo the editor is holding, and taken by handle rather than borrowed,
/// because a render must not hold that lock while it works. The handle is an
/// `Arc`, so this costs nothing beyond the lock it takes and gives back.
fn decode_already_in_hand(state: &crate::AppState, path: &str) -> Option<Arc<DynamicImage>> {
    let held = state.original_image.lock().unwrap();
    held.as_ref()
        .filter(|loaded| loaded.path == path)
        .map(|loaded| loaded.image.clone())
}
// ========== BLITZRAW END: one queue owns every rebuild ==========

#[cfg(test)]
mod rebuild_queue_tests {
    use super::*;
    use crate::app_state::{DirtyPicture, ThumbnailManager};

    fn marked(ago: Duration) -> DirtyPicture {
        let at = Instant::now() - ago;
        DirtyPicture {
            first_marked: at,
            last_marked: at,
        }
    }

    fn mark(manager: &ThumbnailManager, path: &str, entry: DirtyPicture) {
        manager
            .dirty
            .lock()
            .unwrap()
            .insert(path.to_string(), entry);
    }

    /// The whole point of the settle. A press half a second ago is a run that
    /// may not be over, and rendering into the middle of one is the storm.
    #[test]
    fn a_photo_marked_a_moment_ago_is_left_to_settle() {
        let manager = ThumbnailManager::new();
        mark(&manager, "a.nef", marked(Duration::from_millis(20)));

        let (job, _) = claim_next_job(&manager, Instant::now());
        assert!(job.is_none(), "still settling");
    }

    #[test]
    fn a_photo_that_has_gone_quiet_is_rendered() {
        let manager = ThumbnailManager::new();
        mark(
            &manager,
            "a.nef",
            marked(REBUILD_SETTLE + Duration::from_millis(50)),
        );

        let (job, _) = claim_next_job(&manager, Instant::now());
        let job = job.expect("settled");
        assert_eq!(job.path, "a.nef");
        assert!(job.force, "an edit does not trust the file already cached");
        assert!(
            manager.dirty.lock().unwrap().is_empty(),
            "claimed, so no longer waiting"
        );
    }

    /// A key held down keeps pushing the settle back. Without the deadline the
    /// photo would never be rendered while the finger was down.
    #[test]
    fn a_photo_still_being_pressed_renders_anyway_eventually() {
        let manager = ThumbnailManager::new();
        mark(
            &manager,
            "a.nef",
            DirtyPicture {
                first_marked: Instant::now() - (REBUILD_MAX_WAIT + Duration::from_millis(50)),
                last_marked: Instant::now(),
            },
        );

        let (job, _) = claim_next_job(&manager, Instant::now());
        assert!(job.is_some(), "past the deadline, so it renders regardless");
    }

    /// What the reported freeze was: ten presses across a selection, each one
    /// a full render of every photo in it.
    #[test]
    fn ten_presses_on_twenty_eight_photos_are_twenty_eight_renders() {
        let manager = ThumbnailManager::new();
        let photos: Vec<String> = (0..28).map(|n| format!("{n}.nef")).collect();

        let mut now = Instant::now();
        for _press in 0..10 {
            now += Duration::from_millis(40);
            let mut dirty = manager.dirty.lock().unwrap();
            for photo in &photos {
                match dirty.get_mut(photo) {
                    Some(entry) => entry.last_marked = now,
                    None => {
                        dirty.insert(
                            photo.clone(),
                            DirtyPicture {
                                first_marked: now,
                                last_marked: now,
                            },
                        );
                    }
                }
            }
        }

        assert_eq!(
            manager.dirty.lock().unwrap().len(),
            28,
            "ten presses on twenty-eight photos is twenty-eight jobs, not two hundred and eighty"
        );

        // Nothing is claimable until the pressing stops.
        let (job, _) = claim_next_job(&manager, now + Duration::from_millis(40));
        assert!(job.is_none(), "the run is not over yet");

        let mut rendered = 0;
        let settled = now + REBUILD_SETTLE + Duration::from_millis(10);
        while let (Some(job), _) = claim_next_job(&manager, settled) {
            manager.processing_now.lock().unwrap().remove(&job.path);
            rendered += 1;
        }
        assert_eq!(rendered, 28);
    }

    /// A change that arrives while a photo is being rendered must not be lost,
    /// and must not start a second render of the same photo at the same time.
    #[test]
    fn a_photo_being_rendered_is_left_for_the_next_round() {
        let manager = ThumbnailManager::new();
        mark(&manager, "a.nef", marked(REBUILD_SETTLE * 2));
        manager
            .processing_now
            .lock()
            .unwrap()
            .insert("a.nef".to_string());

        let (job, _) = claim_next_job(&manager, Instant::now());
        assert!(job.is_none(), "not rendered twice at once");
        assert!(
            manager.dirty.lock().unwrap().contains_key("a.nef"),
            "and not forgotten either"
        );
    }

    /// Scrolling rewrites the other lane, so an edit waiting there would be
    /// thrown away. It is also what the user just did.
    #[test]
    fn an_edit_is_served_before_a_photo_scrolled_into_view() {
        let manager = ThumbnailManager::new();
        manager
            .queue
            .lock()
            .unwrap()
            .push_back("scrolled.nef".to_string());
        mark(&manager, "edited.nef", marked(REBUILD_SETTLE * 2));

        let (job, _) = claim_next_job(&manager, Instant::now());
        assert_eq!(job.expect("something to do").path, "edited.nef");
    }

    #[test]
    fn a_photo_scrolled_into_view_trusts_the_file_already_cached() {
        let manager = ThumbnailManager::new();
        manager
            .queue
            .lock()
            .unwrap()
            .push_back("scrolled.nef".to_string());

        let (job, skipped) = claim_next_job(&manager, Instant::now());
        let job = job.expect("something to do");
        assert_eq!(job.path, "scrolled.nef");
        assert!(!job.force, "nothing changed it, so a current file will do");
        assert_eq!(skipped, 0);
    }
}

pub fn start_thumbnail_workers(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<crate::AppState>();
    let manager = state.thumbnail_manager.clone();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let thread_count = settings.thumbnail_worker_threads.unwrap_or(4).clamp(1, 16);

    for _ in 0..thread_count {
        let app_clone = app_handle.clone();
        let manager_clone = manager.clone();
        let worker_settings = settings.clone();

        std::thread::spawn(move || {
            loop {
                let (job, skipped) = claim_next_job(&manager_clone, Instant::now());

                // Paths dropped for already being rendered still have to be
                // counted, or the progress bar never reaches its total.
                if skipped > 0 {
                    let state = app_clone.state::<crate::AppState>();
                    for _ in 0..skipped {
                        increment_thumbnail_progress(&state, &app_clone);
                    }
                }

                let Some(job) = job else {
                    // Nothing to do, or a changed photo still settling. A settle
                    // runs out on its own with nothing to announce it, so this
                    // is a timed wait rather than an indefinite one.
                    let queue = manager_clone.queue.lock().unwrap();
                    let _ = manager_clone.cvar.wait_timeout(queue, REBUILD_POLL);
                    continue;
                };

                let state = app_clone.state::<crate::AppState>();
                let gpu_context =
                    crate::gpu_processing::get_or_init_gpu_context(&state, &app_clone).ok();

                if let Ok(cache_dir) = get_thumb_cache_dir(&app_clone) {
                    if manager_clone.rotational_disk.load(Ordering::Relaxed) {
                        let _io_permit = manager_clone.io_gate.lock().unwrap();
                        prefetch_source_file(&job.path);
                    }

                    let in_hand = decode_already_in_hand(&state, &job.path);

                    let result = generate_single_thumbnail_and_cache(
                        &job.path,
                        &cache_dir,
                        gpu_context.as_ref(),
                        in_hand.as_deref(),
                        job.force,
                        &app_clone,
                        &worker_settings,
                    );

                    if let Some((thumbnail_path, rating, is_edited)) = result {
                        emit_thumbnail_generated(
                            &app_clone,
                            &job.path,
                            &thumbnail_path,
                            rating,
                            is_edited,
                        );
                    }
                    increment_thumbnail_progress(&state, &app_clone);
                } else if let Err(e) = get_thumb_cache_dir(&app_clone) {
                    // BLITZRAW: counted and reported rather than dropped. A job
                    // that vanishes without being counted leaves the progress
                    // bar short of its total for the rest of the session, and
                    // the grid waiting for a picture that is never coming.
                    emit_thumbnail_cache_setup_error(&app_clone, &job.path, &e);
                    increment_thumbnail_progress(&state, &app_clone);
                }
                manager_clone
                    .processing_now
                    .lock()
                    .unwrap()
                    .remove(&job.path);
            }
        });
    }
}

#[tauri::command]
pub fn update_thumbnail_queue(
    paths: Vec<String>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let state = app_handle.state::<crate::AppState>();

    let mut queue = state.thumbnail_manager.queue.lock().unwrap();

    if paths.is_empty() {
        queue.clear();
        let mut tracker = state.thumbnail_progress.lock().unwrap();
        tracker.total = 0;
        tracker.completed = 0;
        drop(tracker);

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        state.thumbnail_manager.cvar.notify_all();
        return Ok(());
    }

    let mut unique_paths = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if seen.insert(path.clone()) {
            unique_paths.push(path);
        }
    }

    queue.retain(|p| !seen.contains(p));

    while queue.len() + unique_paths.len() > 500 {
        queue.pop_front();
    }

    if state
        .thumbnail_manager
        .rotational_disk
        .load(Ordering::Relaxed)
    {
        unique_paths.sort();
        for path in unique_paths.into_iter().rev() {
            queue.push_back(path);
        }
    } else {
        for path in unique_paths {
            queue.push_back(path);
        }
    }

    let queue_len = queue.len();
    drop(queue);

    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total = tracker.completed + queue_len;

    let current = tracker.completed;
    let total = tracker.total;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );

    state.thumbnail_manager.cvar.notify_all();
    Ok(())
}

pub fn add_to_thumbnail_queue(state: &AppState, count: usize, app_handle: &AppHandle) {
    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total += count;
    let current = tracker.completed;
    let total = tracker.total;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": current, "total": total }),
    );
}

pub fn increment_thumbnail_progress(state: &AppState, app_handle: &AppHandle) {
    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.completed += 1;
    let current = tracker.completed;
    let total = tracker.total;

    if current >= total {
        tracker.total = 0;
        tracker.completed = 0;
        drop(tracker);

        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": 0, "total": 0 }),
        );
        let _ = app_handle.emit("thumbnail-generation-complete", true);
    } else {
        drop(tracker);
        let _ = app_handle.emit(
            "thumbnail-progress",
            serde_json::json!({ "current": current, "total": total }),
        );
    }
}

// ============ BLITZRAW: a name that never changes needs a version ============
/// A number that changes whenever a thumbnail is rewritten.
///
/// A thumbnail's file name is fixed for the life of the photo, which is the
/// whole point: it survives the photo being moved, renamed or archived. It also
/// means the URL handed to the webview never changes, and a webview that has
/// already fetched a URL does not fetch it again. So an edit rewrote the file
/// on disk correctly and the grid, the filmstrip and the navigator all carried
/// on showing the picture from before the edit.
///
/// The modification time goes on the URL as a query parameter. Tauri's asset
/// protocol reads only `uri().path()`, so the query is ignored on the way in,
/// and an unchanged thumbnail keeps its URL and stays cached while a rewritten
/// one does not. Milliseconds, because a rewrite is a temp file and a rename
/// and always lands on a fresh timestamp.
///
/// Zero when the file cannot be read, which the front end treats as no version
/// rather than as version zero, so a thumbnail is never worse off than before.
fn thumbnail_version(thumbnail_path: &str) -> u64 {
    fs::metadata(thumbnail_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// Tells every window that a photo's picture on disk has been replaced.
///
/// Public because a caller that rendered its own thumbnail, because it already
/// held the decoded pixels, still has to say so. A merge did not, so a rebuilt
/// merge wrote a new thumbnail nobody was ever told about and the grid went on
/// showing the old one for the rest of the session.
pub fn announce_new_thumbnail(app_handle: &AppHandle, path: &str, thumbnail_path: &Path) {
    let (rating, is_edited) = rating_and_edit_state(path, app_handle);
    emit_thumbnail_generated(
        app_handle,
        path,
        &thumbnail_path.to_string_lossy(),
        rating,
        is_edited,
    );
}

/// What the grid draws on a photo besides the picture: its stars, and whether
/// anything has been done to it.
fn rating_and_edit_state(path_str: &str, app_handle: &AppHandle) -> (u8, bool) {
    let (_, sidecar_path) = parse_virtual_path(path_str);
    let meta = crate::exif_processing::load_sidecar(&sidecar_path);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let is_raw = crate::formats::is_raw_file(path_str);
    let tonemapper = crate::image_processing::resolve_tonemapper_override(&settings, is_raw);
    (
        meta.rating,
        crate::image_processing::is_image_edited(&meta.adjustments, is_raw, tonemapper),
    )
}

fn emit_thumbnail_generated(
    app_handle: &AppHandle,
    path: &str,
    thumbnail_path: &str,
    rating: u8,
    is_edited: bool,
) {
    let _ = app_handle.emit(
        "thumbnail-generated",
        serde_json::json!({
            "path": path,
            "thumbnailPath": thumbnail_path,
            // BLITZRAW: so the picture on screen changes when the file does.
            "version": thumbnail_version(thumbnail_path),
            "rating": rating,
            "is_edited": is_edited
        }),
    );
}
// ========== BLITZRAW END: a name that never changes needs a version ==========

// ============ BLITZRAW: scopes without waiting for a decode ============
/// The scopes of a photo, taken from the small picture of it that already
/// exists.
///
/// # Why
///
/// Scopes used to arrive only when a raw had been fully decoded and rendered,
/// which is a second and a half for a Z9 frame. So they were blank while
/// opening a photo, blank in the grid where nothing is being decoded at all,
/// and impossible to use for the thing they are most useful for: flicking along
/// a strip comparing exposure and white balance between frames.
///
/// Something of every photo is already on disk, though. A rendered preview if
/// one has been built, and a thumbnail for anything that has ever been looked
/// at. Both are the photo with its own adjustments in it, encoded to sRGB, so
/// their scopes are the photo's scopes to within the resampling. That is not
/// exact and does not need to be: it is enough to see that one frame is a third
/// of a stop hotter than the next.
///
/// The maths is the same maths. This decodes a small JPEG and hands it to the
/// functions the analytics worker uses, rather than reimplementing four scope
/// renderers somewhere else and having them drift.
///
/// Returns nothing at all for a photo with neither, which the caller reads as
/// "no scopes yet" and not as an error.
#[tauri::command]
pub fn scopes_from_small_picture(
    path: String,
    scopes: Option<String>,
    app_handle: AppHandle,
) -> Result<Option<Value>, String> {
    // The preview first, since it is bigger and therefore a better sample.
    let picture = crate::preview_cache::newest_preview_for(&path).or_else(|| {
        let dir = resolve_thumbnail_cache_dir(&app_handle).ok()?;
        let thumbnail = thumbnail_path_for(&dir, &path);
        thumbnail.exists().then_some(thumbnail)
    });

    let Some(picture) = picture else {
        return Ok(None);
    };

    let bytes = std::fs::read(&picture).map_err(|e| e.to_string())?;
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;

    let histogram = crate::image_processing::calculate_histogram_from_image(&image).ok();
    let waveform = match scopes.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(scopes) => {
            crate::image_processing::calculate_waveform_from_image(&image, Some(scopes)).ok()
        }
        None => None,
    };

    if histogram.is_none() && waveform.is_none() {
        return Ok(None);
    }

    Ok(Some(serde_json::json!({
        "path": path,
        "histogram": histogram,
        "waveform": waveform,
    })))
}
// ========== BLITZRAW END: scopes without waiting for a decode ==========

// ================ BLITZRAW: what left the building is worth keeping ================
/// Marks each photo's current state as one that was exported.
///
/// A pinned step holds a whole state and is never culled, however old, so a
/// hundred edits later it is still possible to get back to exactly what the
/// client was sent. Called when an export finishes rather than when it starts,
/// so a cancelled one pins nothing.
///
/// Through the same write door as everything else, so an adjustment saved at
/// the moment an export finishes is not overwritten by the pin, and the pin is
/// not overwritten by it.
#[tauri::command]
pub fn pin_exported_state(paths: Vec<String>, label: String) -> Result<(), String> {
    let at = chrono::Utc::now().to_rfc3339();
    let mut pinned = 0usize;
    for path in &paths {
        let (_, sidecar_path) = parse_virtual_path(path);
        if !sidecar_path.exists() {
            continue;
        }
        let outcome = crate::sidecar::update(&sidecar_path, |metadata| {
            if metadata.adjustments.is_null() {
                return None;
            }
            let state = metadata.adjustments.clone();
            metadata.history = Some(crate::edit_history::pin(
                metadata.history.take(),
                &state,
                &label,
                "export",
                at.clone(),
            ));
            Some(())
        });
        match outcome {
            Ok(written) => {
                if written.is_some() {
                    pinned += 1;
                }
            }
            Err(e) => log::warn!("Could not describe the sidecar for {path}: {e}"),
        }
    }
    log::info!(
        "Pinned the exported state of {pinned} of {} photos",
        paths.len()
    );
    Ok(())
}
// ============== BLITZRAW END: what left the building is worth keeping ==============

pub fn resolve_lens_params_in_adjustments(
    adjustments: &mut Value,
    exif_data: &Option<HashMap<String, String>>,
    lens_db: Option<&crate::lens_correction::LensDatabase>,
) {
    if let Some(map) = adjustments.as_object_mut() {
        let mode = map
            .get("lensCorrectionMode")
            .and_then(|v| v.as_str())
            .unwrap_or("manual");

        if mode == "auto" {
            if let Some(exif) = exif_data {
                let exif_maker = exif.get("Make").map(|s| s.as_str()).unwrap_or("");
                let exif_model = exif.get("LensModel").map(|s| s.as_str()).unwrap_or("");
                if let Some(db) = lens_db {
                    if let Some((detected_maker, detected_model)) =
                        crate::lens_correction::find_best_lens_match(db, exif_maker, exif_model)
                    {
                        map.insert(
                            "lensMaker".to_string(),
                            serde_json::to_value(&detected_maker).unwrap(),
                        );
                        map.insert(
                            "lensModel".to_string(),
                            serde_json::to_value(&detected_model).unwrap(),
                        );
                    } else {
                        map.remove("lensMaker");
                        map.remove("lensModel");
                    }
                }
            } else {
                map.remove("lensMaker");
                map.remove("lensModel");
            }
        }

        if let Some(db) = lens_db {
            let has_valid_lens = match (
                map.get("lensMaker").and_then(|v| v.as_str()),
                map.get("lensModel").and_then(|v| v.as_str()),
            ) {
                (Some(maker), Some(model)) if !maker.is_empty() && !model.is_empty() => {
                    // ============ BLITZRAW: what the profile is actually asked for ============
                    // A zoom is measured at a handful of focal lengths and the
                    // answer is interpolated between them, so the focal length
                    // is the single most important thing handed over. It used to
                    // start at 50mm and stay there if the photo could not be
                    // read, which on a 14-30 means correcting every frame as
                    // though it were shot at 30, silently. None now means no
                    // correction, which is the honest answer.
                    let mut focal_length: Option<f32> = None;
                    let mut aperture = None;
                    let mut distance = None;

                    if let Some(exif) = exif_data {
                        // The real focal length, not the 35mm equivalent. They
                        // are the same on a full frame body and are not on
                        // anything else, and the profile wants the real one.
                        if let Some(fl_str) = exif
                            .get("FocalLength")
                            .or(exif.get("FocalLengthIn35mmFilm"))
                            && let Ok(fl) = fl_str.replace(" mm", "").trim().parse::<f32>()
                            && fl > 0.0
                        {
                            focal_length = Some(fl);
                        }
                        // FNumber first, because `ApertureValue` is not one.
                        //
                        // It is the APEX number, which is 2*log2(f), and this
                        // reader prints it with an "f/" in front of it as though
                        // it were a real aperture. A frame shot at f/6.3 stores
                        // `ApertureValue` as "f/5.310704". Vignetting is measured
                        // per aperture, so it was being read off the wrong curve.
                        if let Some(ap_str) = exif.get("FNumber").or(exif.get("ApertureValue"))
                            && let Ok(ap) = ap_str.replace("f/", "").trim().parse::<f32>()
                            && ap > 0.0
                        {
                            aperture = Some(ap);
                        }
                        if let Some(dist_str) = exif.get("SubjectDistance")
                            && let Ok(dist) = dist_str.replace(" m", "").trim().parse::<f32>()
                        {
                            distance = Some(dist);
                        }
                    }

                    if let Some(params) = focal_length.and_then(|focal_length| {
                        crate::lens_correction::resolve_lens_params(
                            db,
                            maker,
                            model,
                            focal_length,
                            aperture,
                            distance,
                        )
                    }) {
                        map.insert(
                            "lensDistortionParams".to_string(),
                            serde_json::to_value(params).unwrap(),
                        );
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            };

            if !has_valid_lens {
                map.remove("lensDistortionParams");
            }
        }
    }
}

#[tauri::command]
pub fn get_supported_file_types() -> Result<serde_json::Value, String> {
    let raw_extensions: Vec<&str> = crate::formats::RAW_EXTENSIONS
        .iter()
        .map(|(ext, _)| *ext)
        .collect();
    let non_raw_extensions: Vec<&str> = crate::formats::NON_RAW_EXTENSIONS.to_vec();

    Ok(serde_json::json!({
        "raw": raw_extensions,
        "nonRaw": non_raw_extensions
    }))
}

#[tauri::command]
pub fn create_folder(path: String) -> Result<(), String> {
    let path_obj = Path::new(&path);
    if let (Some(parent), Some(new_folder_name_os)) = (path_obj.parent(), path_obj.file_name())
        && let Some(new_folder_name) = new_folder_name_os.to_str()
        && parent.exists()
    {
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            if let Ok(entry) = entry
                && entry.file_name().to_string_lossy().to_lowercase()
                    == new_folder_name.to_lowercase()
            {
                return Err("A folder with that name already exists.".to_string());
            }
        }
    }
    fs::create_dir_all(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn rename_folder(path: String, new_name: String, app_handle: AppHandle) -> Result<(), String> {
    let p = Path::new(&path);
    if !p.is_dir() {
        return Err("Path is not a directory.".to_string());
    }
    if let Some(parent) = p.parent() {
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            if let Ok(entry) = entry
                && entry.file_name().to_string_lossy().to_lowercase() == new_name.to_lowercase()
                && entry.path() != p
            {
                return Err("A folder with that name already exists.".to_string());
            }
        }
        let new_path = parent.join(&new_name);
        fs::rename(p, &new_path).map_err(|e| e.to_string())?;

        let new_folder_str = new_path.to_string_lossy().into_owned();
        sync_album_path_changes(&app_handle, None, None, Some((&path, &new_folder_str)));

        Ok(())
    } else {
        Err("Could not determine parent directory.".to_string())
    }
}

#[tauri::command]
pub fn delete_folder(path: String, app_handle: AppHandle) -> Result<(), String> {
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    {
        if let Err(trash_error) = trash::delete(&path) {
            log::warn!(
                "Failed to move folder to trash: {}. Falling back to permanent delete.",
                trash_error
            );
            fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
    }

    let mut deletions = HashSet::new();
    deletions.insert(path);
    sync_album_path_changes(&app_handle, None, Some(&deletions), None);

    Ok(())
}

#[tauri::command]
pub fn duplicate_file(
    path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let (source_path, source_sidecar_path) = parse_virtual_path(&path);
    if !source_path.is_file() {
        return Err("Source path is not a file.".to_string());
    }

    let parent = source_path
        .parent()
        .ok_or("Could not get parent directory")?;
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Could not get file stem")?;
    let extension = source_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let mut counter = 1;
    let mut dest_path;
    loop {
        let new_stem = if counter == 1 {
            format!("{}_copy", stem)
        } else {
            format!("{}_copy_{}", stem, counter - 1)
        };
        dest_path = parent.join(format!("{}.{}", new_stem, extension));
        if !dest_path.exists() {
            break;
        }
        counter += 1;
    }

    fs::copy(&source_path, &dest_path).map_err(|e| e.to_string())?;

    if source_sidecar_path.exists()
        && let Some(dest_str) = dest_path.to_str()
    {
        let (_, dest_sidecar_path) = parse_virtual_path(dest_str);
        fs::copy(&source_sidecar_path, &dest_sidecar_path).map_err(|e| e.to_string())?;
    }

    let mut source_rrexif_name = source_path.file_name().unwrap().to_os_string();
    source_rrexif_name.push(".rrexif");
    let source_rrexif = source_path.with_file_name(source_rrexif_name);

    if source_rrexif.exists() {
        let mut dest_rrexif_name = dest_path.file_name().unwrap().to_os_string();
        dest_rrexif_name.push(".rrexif");
        let dest_rrexif = dest_path.with_file_name(dest_rrexif_name);
        let _ = fs::copy(&source_rrexif, &dest_rrexif);
    }

    let dest_path_str = dest_path.to_string_lossy().into_owned();

    if let Some(album_id) = target_album_id {
        let _ = add_to_album(album_id, vec![dest_path_str.clone()], app_handle);
    }

    Ok(dest_path_str)
}

fn find_all_associated_files(source_image_path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut associated_files = vec![source_image_path.to_path_buf()];

    let mut rrexif_name = source_image_path
        .file_name()
        .unwrap_or_default()
        .to_os_string();
    rrexif_name.push(".rrexif");
    let rrexif_path = source_image_path.with_file_name(rrexif_name);

    if rrexif_path.exists() {
        associated_files.push(rrexif_path);
    }

    let parent_dir = source_image_path
        .parent()
        .ok_or("Could not determine parent directory")?;
    let source_filename = source_image_path
        .file_name()
        .ok_or("Could not get source filename")?
        .to_string_lossy();

    let primary_sidecar_name = format!("{}.rrdata", source_filename);
    let virtual_copy_prefix = format!("{}.", source_filename);

    if let Ok(entries) = fs::read_dir(parent_dir) {
        for entry in entries.filter_map(Result::ok) {
            let entry_path = entry.path();
            if !entry_path.is_file() {
                continue;
            }

            let entry_os_filename = entry.file_name();
            let entry_filename = entry_os_filename.to_string_lossy();

            if entry_filename == primary_sidecar_name
                || (entry_filename.starts_with(&virtual_copy_prefix)
                    && entry_filename.ends_with(".rrdata"))
            {
                associated_files.push(entry_path);
            }
        }
    }

    Ok(associated_files)
}

#[tauri::command]
pub fn copy_files(source_paths: Vec<String>, destination_folder: String) -> Result<(), String> {
    let dest_path = Path::new(&destination_folder);
    if !dest_path.is_dir() {
        return Err(format!(
            "Destination is not a folder: {}",
            destination_folder
        ));
    }

    let unique_source_images: HashSet<PathBuf> = source_paths
        .iter()
        .map(|p| parse_virtual_path(p).0)
        .collect();

    for source_image_path in unique_source_images {
        let all_files_to_copy = find_all_associated_files(&source_image_path)?;

        let source_parent = source_image_path
            .parent()
            .ok_or("Could not get parent directory")?;
        if source_parent == dest_path {
            let stem = source_image_path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("Could not get file stem")?;
            let extension = source_image_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("");

            let mut counter = 1;
            let new_base_path = loop {
                let new_stem = format!("{}_copy_{}", stem, counter);
                let temp_path = source_parent.join(format!("{}.{}", new_stem, extension));
                if !temp_path.exists() {
                    break temp_path;
                }
                counter += 1;
            };
            let new_filename = new_base_path.file_name().unwrap().to_string_lossy();

            for original_file in all_files_to_copy {
                let original_full_filename = original_file.file_name().unwrap().to_string_lossy();
                let source_base_filename = source_image_path.file_name().unwrap().to_string_lossy();
                let new_dest_filename =
                    original_full_filename.replacen(&*source_base_filename, &new_filename, 1);
                let final_dest_path = dest_path.join(new_dest_filename);

                fs::copy(&original_file, &final_dest_path).map_err(|e| e.to_string())?;
            }
        } else {
            for file_to_copy in all_files_to_copy {
                if let Some(file_name) = file_to_copy.file_name() {
                    let dest_file_path = dest_path.join(file_name);
                    fs::copy(&file_to_copy, &dest_file_path).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn move_files(
    source_paths: Vec<String>,
    destination_folder: String,
    app_handle: AppHandle,
) -> Result<(), String> {
    let dest_path = Path::new(&destination_folder);
    if !dest_path.is_dir() {
        return Err(format!(
            "Destination is not a folder: {}",
            destination_folder
        ));
    }

    let unique_source_images: HashSet<PathBuf> = source_paths
        .iter()
        .map(|p| parse_virtual_path(p).0)
        .collect();

    let mut all_files_to_trash = Vec::new();
    let mut renames = HashMap::new();

    for source_image_path in unique_source_images {
        let source_parent = source_image_path
            .parent()
            .ok_or("Could not get parent directory")?;
        if source_parent == dest_path {
            return Err("Cannot move files into the same folder they are already in.".to_string());
        }

        let files_to_move = find_all_associated_files(&source_image_path)?;

        for file_to_move in &files_to_move {
            if let Some(file_name) = file_to_move.file_name() {
                let dest_file_path = dest_path.join(file_name);
                if dest_file_path.exists() {
                    return Err(format!(
                        "File already exists at destination: {}",
                        dest_file_path.display()
                    ));
                }
            }
        }

        for file_to_move in &files_to_move {
            if let Some(file_name) = file_to_move.file_name() {
                let dest_file_path = dest_path.join(file_name);
                fs::copy(file_to_move, &dest_file_path).map_err(|e| e.to_string())?;
            }
        }

        let dest_image_path = dest_path.join(source_image_path.file_name().unwrap());
        renames.insert(
            source_image_path.to_string_lossy().into_owned(),
            dest_image_path.to_string_lossy().into_owned(),
        );

        all_files_to_trash.extend(files_to_move);
    }

    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    if !all_files_to_trash.is_empty()
        && let Err(trash_error) = trash::delete_all(&all_files_to_trash)
    {
        log::warn!(
            "Failed to move source files to trash: {}. Falling back to permanent delete.",
            trash_error
        );
        for path in all_files_to_trash {
            if path.is_file() {
                fs::remove_file(&path).map_err(|e| {
                    format!("Failed to delete source file {}: {}", path.display(), e)
                })?;
            }
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    for path in all_files_to_trash {
        if path.is_file() {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to delete source file {}: {}", path.display(), e))?;
        }
    }

    sync_album_path_changes(&app_handle, Some(&renames), None, None);

    Ok(())
}

#[tauri::command]
pub fn save_metadata_and_update_thumbnail(
    path: String,
    adjustments: Value,
    // ================ BLITZRAW: a name for the step, when there is one ================
    // Only the label. Whether this write continues the step at the top or
    // starts a new one is worked out from the stored log itself, because the
    // save and the editor's own history push are separately rate-limited and
    // arrive in whichever order they arrive, so a flag sent from the editor
    // would describe whichever of the two happened to run first.
    //
    // Absent means an ordinary unnamed step, which is what every existing call
    // site sends and what the sliders want.
    history_label: Option<String>,
    // BLITZRAW: the one thing the user did that this write is part of.
    //
    // The same name reaches every photo one action touched, so an application
    // level undo can find them all. Absent means the change belongs to no
    // named action and is recorded exactly as it was before.
    history_action: Option<String>,
    // ============== BLITZRAW END: a name for the step, when there is one ==============
    app_handle: AppHandle,
    state: tauri::State<AppState>,
) -> Result<Option<StepMoved>, String> {
    let (source_path, sidecar_path) = parse_virtual_path(&path);

    let settings = load_settings(app_handle.clone()).ok();
    let lens_db = state.lens_db.lock().unwrap().clone();
    let history_limit = crate::edit_history::usable_step_limit(
        settings.as_ref().and_then(|s| s.history_step_limit),
    );
    let at = chrono::Utc::now().to_rfc3339();
    // BLITZRAW: where this write moved the photo, read inside the same lock as
    // the write itself, so the numbers describe what was actually written.
    let stepped: std::cell::Cell<Option<(u64, u64)>> = std::cell::Cell::new(None);

    // BLITZRAW: read, changed and written inside one lock. Read outside it and
    // the "before" this step is measured against can be a state that has
    // already been replaced, which writes a history entry describing a change
    // that never happened and then saves over the change that did.
    let metadata = crate::sidecar::update(&sidecar_path, |metadata| {
        let mut final_adjustments = adjustments;
        resolve_lens_params_in_adjustments(
            &mut final_adjustments,
            &metadata.exif,
            lens_db.as_deref(),
        );

        // ========== BLITZRAW: the history goes out in this same write ==========
        // Not from a second writer racing this one. See edit_history.
        let from = crate::edit_history::bookmark(metadata.history.as_ref());
        metadata.history = Some(crate::edit_history::record(
            metadata.history.take(),
            &metadata.adjustments,
            &final_adjustments,
            crate::edit_history::Recording {
                label: history_label.as_deref(),
                action: history_action.as_deref(),
                limit: history_limit,
                at,
            },
        ));
        stepped.set(Some((
            from,
            metadata.history.as_ref().map(|h| h.at).unwrap_or(from),
        )));
        // ======== BLITZRAW END: the history goes out in this same write ========

        metadata.adjustments = final_adjustments;
        Some(metadata.clone())
    })
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "The save changed nothing".to_string())?;

    if let Some(settings) = settings.as_ref()
        && settings.enable_xmp_sync.unwrap_or(false)
    {
        let create_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        sync_metadata_to_xmp(&source_path, &metadata, create_if_missing);
    }

    let loaded_image_lock = state.original_image.lock().unwrap();
    let preloaded_image_option = if let Some(loaded_image) = loaded_image_lock.as_ref() {
        if loaded_image.path == path {
            Some(loaded_image.image.clone())
        } else {
            None
        }
    } else {
        None
    };
    drop(loaded_image_lock);

    let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();
    let app_handle_clone = app_handle.clone();
    let path_clone = path.clone();

    add_to_thumbnail_queue(&state, 1, &app_handle);

    thread::spawn(move || {
        let state = app_handle_clone.state::<AppState>();
        let settings = load_settings(app_handle_clone.clone()).unwrap_or_default();

        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle_clone) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!(
                    "Unable to initialize thumbnail cache directory for '{}': {}",
                    path_clone,
                    e
                );
                emit_thumbnail_cache_setup_error(&app_handle_clone, &path_clone, &e);
                increment_thumbnail_progress(&state, &app_handle_clone);
                return;
            }
        };

        let result = generate_single_thumbnail_and_cache(
            &path_clone,
            &thumb_cache_dir,
            gpu_context.as_ref(),
            preloaded_image_option.as_deref(),
            true,
            &app_handle_clone,
            &settings,
        );

        if let Some((thumbnail_path, rating, is_edited)) = result {
            emit_thumbnail_generated(
                &app_handle_clone,
                &path_clone,
                &thumbnail_path,
                rating,
                is_edited,
            );
        }

        increment_thumbnail_progress(&state, &app_handle_clone);
    });

    // BLITZRAW: which numbers this write moved the photo between, so the
    // application can record one entry and undo it later without holding a
    // single value of its own.
    Ok(stepped.get().map(|(from, to)| StepMoved {
        path,
        from,
        to,
    }))
}

// ================== BLITZRAW: quick adjustments ==================

/// Nudges one adjustment on a set of files without opening any of them.
///
/// Quick Adjustments works on the live image in the editor, but its point is
/// the grid: run down a shoot bumping exposure a tenth at a time without
/// stopping to open anything. That means a read, modify and write per file
/// rather than one value pushed to all of them, since each starts somewhere
/// different.
///
/// `path` is dotted, so nested settings such as `whiteBalance.kelvin` are
/// reachable. `fallback` is what to use when a file has never had the setting
/// touched, since defaults live in the front end.
///
/// White balance is the exception: absent means as-shot, which is a property of
/// the file rather than a constant, so it is read from the camera profile.
// ============ BLITZRAW: moving photos to the steps they were on ============
// The other half of an application level undo. The application knows the order
// things were done in and, for each photo, the number it moved from and the
// number it moved to. It holds no values at all.
//
// So this is not a fan-out. A fan-out sends one photo's values to all the others
// and overwrites theirs. This moves each photo's own bookmark to a number of its
// own, and each of them looks up what that means in its own file. They look
// alike and they are opposite.

/// Where a write left a photo: the number it was on, and the number it is on now.
///
/// The application's list of what I did is made of these and nothing else. Two
/// numbers per photo, so an undo sets the bookmark to `from` and a redo sets it
/// to `to`, with no searching and no values held anywhere but the photo's own
/// file.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StepMoved {
    pub path: String,
    pub from: u64,
    pub to: u64,
}

/// One photo, and the number to put its bookmark on.
#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StepTarget {
    pub path: String,
    pub n: u64,
}

/// A photo that could not be moved, and why.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Refused {
    pub path: String,
    pub reason: crate::edit_history::CannotGo,
}

/// What happened to each photo.
#[derive(serde::Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct StepReport {
    /// Photos whose bookmark moved.
    pub moved: Vec<String>,
    /// Photos that could not go, each with the reason.
    pub refused: Vec<Refused>,
}

/// Moves each photo's bookmark to the number it is given.
///
/// The log is not touched. Nothing is added and nothing is deleted: the step a
/// photo leaves is still there, which is what makes a redo possible and what
/// makes this different from writing an undo on top of it.
///
/// A photo that cannot go is reported rather than guessed at.
#[tauri::command]
pub async fn go_to_steps(
    targets: Vec<StepTarget>,
    app_handle: AppHandle,
) -> Result<StepReport, String> {
    let report = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        let moved = std::sync::Mutex::new(Vec::new());
        let refused = std::sync::Mutex::new(Vec::new());

        targets.par_iter().for_each(|target| {
            let (_, sidecar_path) = parse_virtual_path(&target.path);
            let written = crate::sidecar::update(&sidecar_path, |metadata| {
                let Some(history) = metadata.history.as_mut() else {
                    refused.lock().unwrap().push(Refused {
                        path: target.path.clone(),
                        reason: crate::edit_history::CannotGo::NoHistory,
                    });
                    // Nothing to write, so nothing is written. See sidecar.rs.
                    return None;
                };
                match crate::edit_history::go_to(history, target.n) {
                    Ok(landed) => {
                        metadata.adjustments = landed.state;
                        Some(metadata.clone())
                    }
                    Err(reason) => {
                        refused.lock().unwrap().push(Refused {
                            path: target.path.clone(),
                            reason,
                        });
                        None
                    }
                }
            });

            if let (true, Ok(Some(metadata))) =
                (enable_xmp_sync, written.as_ref().map(|w| w.as_ref()))
            {
                let source_path = parse_virtual_path(&target.path).0;
                sync_metadata_to_xmp(&source_path, metadata, create_xmp_if_missing);
            }
            if matches!(written, Ok(Some(_))) {
                moved.lock().unwrap().push(target.path.clone());
            }
        });

        let moved = moved.into_inner().unwrap();
        // Queued rather than rendered here, the same way a bulk apply is.
        mark_pictures_changed(&app_handle, &moved);
        StepReport {
            moved,
            refused: refused.into_inner().unwrap(),
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(report)
}
// ========== BLITZRAW END: moving photos to the steps they were on ==========

#[tauri::command]
pub async fn nudge_adjustments_for_paths(
    paths: Vec<String>,
    path: String,
    delta: f64,
    min: f64,
    max: f64,
    fallback: Option<f64>,
    // BLITZRAW: the one thing the user did that this write is part of.
    //
    // The same name reaches every photo one action touched, so an application
    // level undo can find them all. Absent means the change belongs to no
    // named action and is recorded exactly as it was before.
    history_action: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<StepMoved>, String> {
    let changed = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        let lens_db = app_handle
            .state::<AppState>()
            .lens_db
            .lock()
            .unwrap()
            .clone();
        let segments: Vec<&str> = path.split('.').collect();
        let history_limit = crate::edit_history::usable_step_limit(settings.history_step_limit);
        let changed = std::sync::atomic::AtomicUsize::new(0);
        // Which files actually moved, so only those are queued for a new
        // picture. A press that ran into a limit changed nothing and is not a
        // reason to decode a raw.
        let moved: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        // BLITZRAW: where each photo was and where it went, for the
        // application's list of what I did.
        let stepped: std::sync::Mutex<Vec<StepMoved>> = std::sync::Mutex::new(Vec::new());

        // BLITZRAW: read, changed and written inside one lock, per file.
        // Without it, a press still in flight and the press after it both read
        // the same exposure and both wrote the same answer, so a photo pressed
        // ten times moved nine. Measured worse than that under contention: of
        // forty presses at once on one file, one landed. See sidecar.rs.
        paths.par_iter().for_each(|image_path| {
            let (_, sidecar_path) = parse_virtual_path(image_path);
            let written = crate::sidecar::update(&sidecar_path, |metadata| {
                let mut adjustments = std::mem::take(&mut metadata.adjustments);
                if adjustments.is_null() {
                    adjustments = serde_json::json!({});
                }
                // Kept before the press moves it: the sidecar is the only copy
                // of what this photo looked like a moment ago.
                let before = adjustments.clone();

                let current = read_nested_number(&adjustments, &segments).or_else(|| {
                    // As-shot is a property of the file, so ask the profile rather
                    // than starting every photo from the same number.
                    if path == "whiteBalance.kelvin" {
                        let source = parse_virtual_path(image_path).0;
                        crate::camera_profile::profile_for(&source.to_string_lossy())
                            .map(|profile| profile.as_shot_temp_tint().0 as f64)
                    } else {
                        fallback
                    }
                });

                let Some(current) = current else {
                    return None;
                };
                let next = nudged_value(current, delta, min, max);
                if (next - current).abs() < 1e-9 {
                    return None;
                }

                if !write_nested_number(&mut adjustments, &segments, next) {
                    return None;
                }

                // A Kelvin written without its tint would read as a white balance
                // with half its meaning, so seed the tint from the file too.
                if path == "whiteBalance.kelvin"
                    && read_nested_number(&adjustments, &["whiteBalance", "tint"]).is_none()
                {
                    let source = parse_virtual_path(image_path).0;
                    let tint = crate::camera_profile::profile_for(&source.to_string_lossy())
                        .map(|profile| profile.as_shot_temp_tint().1 as f64)
                        .unwrap_or(0.0);
                    write_nested_number(&mut adjustments, &["whiteBalance", "tint"], tint);
                }

                // The real database, not None. Passing None here made the auto
                // branch find no lens and strip lensMaker, lensModel and the
                // profile from every file it touched, so nudging exposure quietly
                // undid lens correction.
                resolve_lens_params_in_adjustments(
                    &mut adjustments,
                    &metadata.exif,
                    lens_db.as_deref(),
                );

                // BLITZRAW: a press is a step, on these photos as much as on
                // the one being looked at.
                //
                // This wrote the new exposure and recorded nothing, so a photo
                // whose only edit was a nudge from the grid showed "Initial
                // state" with the edit plainly applied, and no way to undo it.
                // Thirty-one of the hundred and seventy-one photos in the shoot
                // this was found on were in exactly that state.
                //
                // Unnamed, so the editor names it from what moved, and so a run
                // of presses joins into one step the way a run on the open
                // photo does. Recorded inside the same write as the adjustment,
                // which is the whole reason the log lives in the sidecar.
                let from = crate::edit_history::bookmark(metadata.history.as_ref());
                metadata.history = Some(crate::edit_history::record(
                    metadata.history.take(),
                    &before,
                    &adjustments,
                    crate::edit_history::Recording {
                        label: None,
                        // BLITZRAW: the same name every photo in this press
                        // gets, so one press is one action across all of them.
                        action: history_action.as_deref(),
                        limit: history_limit,
                        at: chrono::Utc::now().to_rfc3339(),
                    },
                ));

                let to = metadata.history.as_ref().map(|h| h.at).unwrap_or(from);
                metadata.adjustments = adjustments;
                Some((metadata.clone(), from, to))
            });

            if let Ok(Some((metadata, from, to))) = written {
                changed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                moved.lock().unwrap().push(image_path.clone());
                // BLITZRAW: the two numbers the application records, so this
                // press can be taken back later without it holding a value.
                stepped.lock().unwrap().push(StepMoved {
                    path: image_path.clone(),
                    from,
                    to,
                });
                if enable_xmp_sync {
                    let source_path = parse_virtual_path(image_path).0;
                    sync_metadata_to_xmp(&source_path, &metadata, create_xmp_if_missing);
                }
            }
        });

        // BLITZRAW: queued, not rendered here.
        //
        // This used to render every file in the selection on the spot, across
        // every core, once per key press. Ten presses on twenty-eight photos
        // was two hundred and eighty raw decodes overlapping, several gigabytes
        // of float images, and a window that stopped answering for a couple of
        // minutes. The queue merges them: the same ten presses now cost
        // twenty-eight renders, once the pressing stops.
        mark_pictures_changed(&app_handle, &moved.lock().unwrap());

        let _ = changed;
        stepped.into_inner().unwrap()
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(changed)
}

fn read_nested_number(value: &Value, segments: &[&str]) -> Option<f64> {
    let mut current = value;
    for segment in segments {
        current = current.get(segment)?;
    }
    current.as_f64()
}

fn write_nested_number(value: &mut Value, segments: &[&str], number: f64) -> bool {
    let Some((last, parents)) = segments.split_last() else {
        return false;
    };
    let mut current = value;
    for segment in parents {
        if !current
            .get(*segment)
            .map(|v| v.is_object())
            .unwrap_or(false)
        {
            let Some(map) = current.as_object_mut() else {
                return false;
            };
            map.insert((*segment).to_string(), serde_json::json!({}));
        }
        current = current.get_mut(*segment).unwrap();
    }
    let Some(map) = current.as_object_mut() else {
        return false;
    };
    map.insert((*last).to_string(), serde_json::json!(number));
    true
}

/// One press, landed on the same grid the slider uses.
///
/// The live path in `quickAdjustments.ts` snaps to the step, so repeated
/// presses stay on values the slider could have produced. This path did not,
/// so two files nudged together drifted apart in the fourth decimal and read
/// as though they were moving independently. `delta` is the step with a sign,
/// so the step is its magnitude.
fn nudged_value(current: f64, delta: f64, min: f64, max: f64) -> f64 {
    let next = (current + delta).clamp(min, max);
    let step = delta.abs();
    if !step.is_finite() || step <= 0.0 {
        return next;
    }
    // Four decimals, matching the front end, so nothing like
    // 0.30000000000000004 ever reaches a sidecar. Clamped again because
    // rounding to the grid can step past a limit that is not a multiple of it.
    let snapped = ((next / step).round() * step * 10_000.0).round() / 10_000.0;
    snapped.clamp(min, max)
}

#[cfg(test)]
mod quick_adjustment_tests {
    use super::nudged_value;

    #[test]
    fn repeated_presses_stay_on_the_step_grid() {
        // The float sum on its own reaches 0.30000000000000004 by the third
        // press, which is the drift that made two files look out of step.
        let mut value = 0.0;
        for expected in [0.1, 0.2, 0.3, 0.4, 0.5] {
            value = nudged_value(value, 0.1, -5.0, 5.0);
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn a_value_off_the_grid_is_pulled_onto_it() {
        // Matches the front end: add the step, then round to the grid. A file
        // sitting at 0.37 from a preset joins the grid rather than carrying its
        // offset forever.
        assert_eq!(nudged_value(0.37, 0.1, -5.0, 5.0), 0.5);
        assert_eq!(nudged_value(0.37, -0.1, -5.0, 5.0), 0.3);
    }

    #[test]
    fn kelvin_lands_on_fifties() {
        assert_eq!(nudged_value(4772.0, 50.0, 1667.0, 50000.0), 4800.0);
        assert_eq!(nudged_value(4800.0, 50.0, 1667.0, 50000.0), 4850.0);
        assert_eq!(nudged_value(4800.0, -50.0, 1667.0, 50000.0), 4750.0);
    }

    #[test]
    fn limits_hold_even_when_they_are_not_multiples_of_the_step() {
        assert_eq!(nudged_value(4.95, 0.3, -5.0, 5.0), 5.0);
        assert_eq!(nudged_value(-4.95, -0.3, -5.0, 5.0), -5.0);
        // Already at the limit, so the caller sees no change and skips the file.
        assert_eq!(nudged_value(5.0, 0.1, -5.0, 5.0), 5.0);
        assert_eq!(nudged_value(45.0, 0.1, -45.0, 45.0), 45.0);
    }

    #[test]
    fn a_zero_step_is_left_alone_rather_than_dividing_by_it() {
        assert_eq!(nudged_value(1.5, 0.0, -5.0, 5.0), 1.5);
    }
}

// ================ BLITZRAW END: quick adjustments ================

#[tauri::command]
pub async fn apply_adjustments_to_paths(
    paths: Vec<String>,
    adjustments: Value,
    // ============ BLITZRAW: the photo the editor is already recording ============
    // A selection usually contains the photo on screen, and that one's history
    // is the editor's to keep: it pushes its own step and saves it a moment
    // later. Recording one here as well would give that photo the same change
    // twice. Every other photo in the selection has no editor watching it, so
    // this is the only thing that can record what happened to them.
    skip_history_for: Option<String>,
    // BLITZRAW: the one thing the user did that this write is part of.
    //
    // The same name reaches every photo one action touched, so an application
    // level undo can find them all. Absent means the change belongs to no
    // named action and is recorded exactly as it was before.
    history_action: Option<String>,
    // ========== BLITZRAW END: the photo the editor is already recording ==========
    app_handle: AppHandle,
) -> Result<Vec<StepMoved>, String> {
    let state = app_handle.state::<AppState>();

    // BLITZRAW: claim this round. A round that a newer one has already
    // superseded writes nothing: what it would write is a subset of what the
    // newer one carries, since every value sent here is absolute rather than a
    // difference. The progress total is added by `mark_pictures_changed`, once
    // per photo that really changed.
    let generation = state
        .apply_adjustments_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;

    let stepped = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

        let lens_db = app_handle
            .state::<AppState>()
            .lens_db
            .lock()
            .unwrap()
            .clone();

        // Only the files that took the paste get a new picture.
        let moved: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        // BLITZRAW: where each photo was and where it went, for the
        // application's list of what I did.
        let stepped: std::sync::Mutex<Vec<StepMoved>> = std::sync::Mutex::new(Vec::new());

        paths.par_iter().for_each(|path| {
            // A newer round is already covering these same files with values
            // that supersede these, so writing them would only be undone.
            if app_handle
                .state::<AppState>()
                .apply_adjustments_generation
                .load(std::sync::atomic::Ordering::SeqCst)
                != generation
            {
                return;
            }

            let (_, sidecar_path) = parse_virtual_path(path);

            // BLITZRAW: read, changed and written inside one lock. The "before"
            // this step is measured against has to be the state that is about
            // to be replaced, not one read a moment earlier and since
            // overwritten. See sidecar.rs.
            let written = crate::sidecar::update(&sidecar_path, |existing_metadata| {
                let mut new_adjustments = std::mem::take(&mut existing_metadata.adjustments);
                if new_adjustments.is_null() {
                    new_adjustments = serde_json::json!({});
                }
                // BLITZRAW: kept before the paste goes over it, because it is the
                // only copy of what this photo looked like a moment ago.
                let before = new_adjustments.clone();

                if let (Some(new_map), Some(pasted_map)) =
                    (new_adjustments.as_object_mut(), adjustments.as_object())
                {
                    for (k, v) in pasted_map {
                        new_map.insert(k.clone(), v.clone());
                    }
                }

                resolve_lens_params_in_adjustments(
                    &mut new_adjustments,
                    &existing_metadata.exif,
                    lens_db.as_deref(),
                );

                // ======= BLITZRAW: what happened to a photo nobody was watching =======
                // The photo's own sidecar is the only place its "before" state
                // exists, and this is about to overwrite it. So the step is
                // recorded here, in the same write, which is the whole reason the
                // log lives in the sidecar rather than in the editor.
                //
                // Never joined to the step before it. A bulk apply is one discrete
                // event however fast it follows another.
                let from = crate::edit_history::bookmark(existing_metadata.history.as_ref());
                if skip_history_for.as_deref() != Some(path.as_str()) {
                    existing_metadata.history = Some(crate::edit_history::record(
                        existing_metadata.history.take(),
                        &before,
                        &new_adjustments,
                        crate::edit_history::Recording {
                            // BLITZRAW: no name, so these read like every other
                            // step.
                            //
                            // A stored name always beats the one the editor
                            // works out from what moved, so stamping every
                            // photo in a selection with the constant "Applied
                            // to a selection" gave each of them a history of
                            // identical lines, where the photo being looked at
                            // got "Exposure", "White Balance" and so on. The
                            // same information is on disk for all of them:
                            // `record` stores which keys moved and by how much,
                            // for every photo it writes.
                            //
                            // A name also forbids joining, so a run of
                            // auto-sync flushes wrote one step per flush on the
                            // other photos while the open one coalesced them
                            // into a single move. Dropping the name fixes the
                            // count as well as the words.
                            label: None,
                            // BLITZRAW: the name is what ties these photos to
                            // each other and to the one in the editor, so an
                            // undo can find every file one action wrote. The
                            // clock above cannot: it is minted per photo inside
                            // this loop, and it moves again whenever a change
                            // joins the step above it.
                            action: history_action.as_deref(),
                            limit: crate::edit_history::usable_step_limit(
                                settings.history_step_limit,
                            ),
                            at: chrono::Utc::now().to_rfc3339(),
                        },
                    ));
                }
                // ===== BLITZRAW END: what happened to a photo nobody was watching =====

                let to = existing_metadata
                    .history
                    .as_ref()
                    .map(|h| h.at)
                    .unwrap_or(from);
                existing_metadata.adjustments = new_adjustments;
                Some((existing_metadata.clone(), from, to))
            });

            if let Ok(Some((metadata, from, to))) = &written {
                if enable_xmp_sync {
                    let source_path = parse_virtual_path(path).0;
                    sync_metadata_to_xmp(&source_path, metadata, create_xmp_if_missing);
                }
                moved.lock().unwrap().push(path.clone());
                // BLITZRAW: the two numbers the application records for this
                // photo, so this change can be taken back later without the
                // application holding a value of its own.
                stepped.lock().unwrap().push(StepMoved {
                    path: path.clone(),
                    from: *from,
                    to: *to,
                });
            }
        });

        // BLITZRAW: queued rather than rendered here. Auto-sync can fire this
        // command every three quarters of a second while a slider is moving,
        // and each round used to re-decode the whole selection. See
        // `mark_pictures_changed`.
        //
        // The generation counter above is what stops a superseded round from
        // being written at all; the queue is what stops two rounds from being
        // rendered twice.
        mark_pictures_changed(&app_handle, &moved.lock().unwrap());
        stepped.into_inner().unwrap()
    })
    .await
    .map_err(|e| e.to_string())?;

    // BLITZRAW: which numbers each photo moved between, so the application can
    // record one entry for this change and undo it later without holding any of
    // the values itself.
    Ok(stepped)
}

#[tauri::command]
pub async fn reset_adjustments_for_paths(
    paths: Vec<String>,
    // ============ BLITZRAW: a reset is a step like anything else ============
    // It was not. This wrote every photo and recorded nothing at all, so a
    // reset could not be taken back on any photo except the one in the editor,
    // which pushed its own step. That is also how an entry in the application's
    // list could come to point at a step that is no longer the step it meant:
    // a write nobody recorded is a write nobody can account for.
    //
    // The rule, in one line: anything that writes a step into a photo writes an
    // entry into the application's list.
    skip_history_for: Option<String>,
    // ========== BLITZRAW END: a reset is a step like anything else ==========
    app_handle: AppHandle,
) -> Result<Vec<StepMoved>, String> {
    let stepped = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        let history_limit = crate::edit_history::usable_step_limit(settings.history_step_limit);
        let stepped: std::sync::Mutex<Vec<StepMoved>> = std::sync::Mutex::new(Vec::new());

        paths.par_iter().for_each(|path| {
            let (_, sidecar_path) = parse_virtual_path(path);

            let written = crate::sidecar::update(&sidecar_path, |existing_metadata| {
                let before = std::mem::take(&mut existing_metadata.adjustments);
                let after = serde_json::json!({});
                let from = crate::edit_history::bookmark(existing_metadata.history.as_ref());
                let mut to = from;
                if skip_history_for.as_deref() != Some(path.as_str()) {
                    existing_metadata.history = Some(crate::edit_history::record(
                        existing_metadata.history.take(),
                        &before,
                        &after,
                        crate::edit_history::Recording {
                            // Named, because a list of the forty things a reset
                            // moved is not what happened. One word is.
                            label: Some("Reset"),
                            action: None,
                            limit: history_limit,
                            at: chrono::Utc::now().to_rfc3339(),
                        },
                    ));
                    to = existing_metadata
                        .history
                        .as_ref()
                        .map(|h| h.at)
                        .unwrap_or(from);
                }
                existing_metadata.adjustments = after;
                Some((existing_metadata.clone(), from, to))
            });

            if let Ok(Some((metadata, from, to))) = &written {
                if enable_xmp_sync {
                    let source_path = parse_virtual_path(path).0;
                    sync_metadata_to_xmp(&source_path, metadata, create_xmp_if_missing);
                }
                stepped.lock().unwrap().push(StepMoved {
                    path: path.clone(),
                    from: *from,
                    to: *to,
                });
            }
        });

        mark_pictures_changed(&app_handle, &paths);

        let state = app_handle.state::<AppState>();
        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!("Unable to initialize thumbnail cache directory: {}", e);
                for path in &paths {
                    emit_thumbnail_cache_setup_error(&app_handle, path, &e);
                }
                for _ in 0..paths.len() {
                    increment_thumbnail_progress(&state, &app_handle);
                }
                // The photos were written and their steps recorded; only the
                // pictures could not be rebuilt. Report the moves anyway, or an
                // undo would have nothing to undo.
                return stepped.into_inner().unwrap();
            }
        };

        let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();

        paths.par_iter().for_each(|path_str| {
            let result = generate_single_thumbnail_and_cache(
                path_str,
                &thumb_cache_dir,
                gpu_context.as_ref(),
                None,
                true,
                &app_handle,
                &settings,
            );

            if let Some((thumbnail_path, rating, is_edited)) = result {
                emit_thumbnail_generated(&app_handle, path_str, &thumbnail_path, rating, is_edited);
            }

            increment_thumbnail_progress(&state, &app_handle);
        });
        stepped.into_inner().unwrap()
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(stepped)
}

#[tauri::command]
pub async fn apply_auto_adjustments_to_paths(
    paths: Vec<String>,
    // BLITZRAW: the open photo pushes its own step, the same as a paste or a
    // reset, so recording one for it here as well would give it the change twice.
    skip_history_for: Option<String>,
    app_handle: AppHandle,
) -> Result<Vec<StepMoved>, String> {
    let state = app_handle.state::<AppState>();
    add_to_thumbnail_queue(&state, paths.len(), &app_handle);

    let stepped = tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
        let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);
        let history_limit = crate::edit_history::usable_step_limit(settings.history_step_limit);
        // BLITZRAW: where each photo was and where it went. This wrote every
        // photo and recorded nothing at all, the same hole Reset had, so an auto
        // adjustment could not be taken back on any photo but the open one.
        let stepped: std::sync::Mutex<Vec<StepMoved>> = std::sync::Mutex::new(Vec::new());

        let state = app_handle.state::<AppState>();
        let thumb_cache_dir = match resolve_thumbnail_cache_dir(&app_handle) {
            Ok(dir) => dir,
            Err(e) => {
                log::warn!("Unable to initialize thumbnail cache directory: {}", e);
                for path in &paths {
                    emit_thumbnail_cache_setup_error(&app_handle, path, &e);
                }
                for _ in 0..paths.len() {
                    increment_thumbnail_progress(&state, &app_handle);
                }
                return stepped.into_inner().unwrap();
            }
        };

        let gpu_context = gpu_processing::get_or_init_gpu_context(&state, &app_handle).ok();

        paths.par_iter().for_each(|path| {
            let loaded_image: Option<DynamicImage> = (|| -> Result<DynamicImage, String> {
                let (source_path, sidecar_path) = parse_virtual_path(path);
                let source_path_str = source_path.to_string_lossy().to_string();

                let file_bytes = fs::read(&source_path).map_err(|e| e.to_string())?;
                let image = image_loader::load_base_image_from_bytes(
                    &file_bytes,
                    &source_path_str,
                    true,
                    &settings,
                    None,
                )
                .map_err(|e| e.to_string())?;

                let auto_results = perform_auto_analysis(&image);
                let auto_adjustments_json = auto_results_to_json(&auto_results);

                let written = crate::sidecar::update(&sidecar_path, |existing_metadata| {
                    if existing_metadata.adjustments.is_null() {
                        existing_metadata.adjustments = serde_json::json!({});
                    }
                    // Kept before the auto values go over it: the only copy of
                    // what this photo looked like a moment ago.
                    let before = existing_metadata.adjustments.clone();
                    let from = crate::edit_history::bookmark(existing_metadata.history.as_ref());

                    if let (Some(existing_map), Some(auto_map)) = (
                        existing_metadata.adjustments.as_object_mut(),
                        auto_adjustments_json.as_object(),
                    ) {
                        for (k, v) in auto_map {
                            if k == "sectionVisibility" {
                                if let Some(existing_vis_val) = existing_map.get_mut(k) {
                                    if let (Some(existing_vis), Some(auto_vis)) =
                                        (existing_vis_val.as_object_mut(), v.as_object())
                                    {
                                        for (vis_k, vis_v) in auto_vis {
                                            existing_vis.insert(vis_k.clone(), vis_v.clone());
                                        }
                                    }
                                } else {
                                    existing_map.insert(k.clone(), v.clone());
                                }
                            } else {
                                existing_map.insert(k.clone(), v.clone());
                            }
                        }
                    }

                    let mut to = from;
                    if skip_history_for.as_deref() != Some(path.as_str()) {
                        let after = existing_metadata.adjustments.clone();
                        existing_metadata.history = Some(crate::edit_history::record(
                            existing_metadata.history.take(),
                            &before,
                            &after,
                            crate::edit_history::Recording {
                                // Named, because the list of forty things an
                                // auto adjustment moves is not what happened.
                                label: Some("Auto adjustments"),
                                action: None,
                                limit: history_limit,
                                at: chrono::Utc::now().to_rfc3339(),
                            },
                        ));
                        to = existing_metadata
                            .history
                            .as_ref()
                            .map(|h| h.at)
                            .unwrap_or(from);
                    }
                    Some((existing_metadata.clone(), from, to))
                });

                if let Ok(Some((metadata, from, to))) = &written {
                    if enable_xmp_sync {
                        sync_metadata_to_xmp(&source_path, metadata, create_xmp_if_missing);
                    }
                    stepped.lock().unwrap().push(StepMoved {
                        path: path.clone(),
                        from: *from,
                        to: *to,
                    });
                }
                Ok(image)
            })()
            .map_err(|e| eprintln!("Failed to apply auto adjustments to {}: {}", path, e))
            .ok();

            let result = generate_single_thumbnail_and_cache(
                path,
                &thumb_cache_dir,
                gpu_context.as_ref(),
                loaded_image.as_ref(),
                true,
                &app_handle,
                &settings,
            );

            if let Some((thumbnail_path, rating, is_edited)) = result {
                emit_thumbnail_generated(&app_handle, path, &thumbnail_path, rating, is_edited);
            }

            increment_thumbnail_progress(&state, &app_handle);
        });
        stepped.into_inner().unwrap()
    })
    .await
    .map_err(|e| e.to_string())?;

    Ok(stepped)
}

#[tauri::command]
pub fn set_color_label_for_paths(
    paths: Vec<String>,
    color: Option<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    paths.par_iter().for_each(|path| {
        let (_, sidecar_path) = parse_virtual_path(path);

        let written = crate::sidecar::update(&sidecar_path, |metadata| {
            let mut tags = metadata.tags.take().unwrap_or_default();
            tags.retain(|tag| !tag.starts_with(COLOR_TAG_PREFIX));

            if let Some(c) = &color
                && !c.is_empty()
            {
                tags.push(format!("{}{}", COLOR_TAG_PREFIX, c));
            }

            metadata.tags = if tags.is_empty() { None } else { Some(tags) };
            Some(metadata.clone())
        });

        if let (true, Ok(Some(metadata))) = (enable_xmp_sync, written) {
            let source_path = parse_virtual_path(path).0;
            sync_metadata_to_xmp(&source_path, &metadata, create_xmp_if_missing);
        }
    });

    Ok(())
}

#[tauri::command]
pub fn set_rating_for_paths(
    paths: Vec<String>,
    rating: u8,
    app_handle: AppHandle,
) -> Result<(), String> {
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);
    let create_xmp_if_missing = settings.create_xmp_if_missing.unwrap_or(false);

    paths.par_iter().for_each(|path| {
        let (_, sidecar_path) = parse_virtual_path(path);

        let written = crate::sidecar::update(&sidecar_path, |metadata| {
            // Nothing to write if the star is already the star it has. Setting
            // a rating to what it already is used to be a new modification
            // time, and a new modification time is every cached picture of the
            // photo thrown away.
            if metadata.rating == rating {
                return None;
            }
            metadata.rating = rating;
            Some(metadata.clone())
        });

        if let (true, Ok(Some(metadata))) = (enable_xmp_sync, written) {
            let source_path = parse_virtual_path(path).0;
            sync_metadata_to_xmp(&source_path, &metadata, create_xmp_if_missing);
        }
    });

    Ok(())
}

#[tauri::command]
pub fn load_metadata(path: String, app_handle: AppHandle) -> Result<ImageMetadata, String> {
    let settings = load_settings(app_handle).unwrap_or_default();
    let enable_xmp_sync = settings.enable_xmp_sync.unwrap_or(false);

    let (source_path, sidecar_path) = parse_virtual_path(&path);
    let mut metadata = crate::exif_processing::load_sidecar(&sidecar_path);

    let mut changed = false;
    if enable_xmp_sync {
        changed |= sync_metadata_from_xmp(&source_path, &mut metadata);

        // BLITZRAW: the camera's own stars, asked for once per photo.
        changed |= sync_metadata_from_embedded_xmp(&source_path, &mut metadata);
    }

    if changed {
        let _ = write_synced_fields(&sidecar_path, &metadata);
    }

    Ok(metadata)
}

fn get_presets_path(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    // BLITZRAW: one data directory, chosen and proved. See data_dir.rs.
    let presets_dir = crate::data_dir::data_path(app_handle, "presets");

    if !presets_dir.exists() {
        fs::create_dir_all(&presets_dir).map_err(|e| e.to_string())?;
    }

    Ok(presets_dir.join("presets.json"))
}

#[tauri::command]
pub fn load_presets(app_handle: AppHandle) -> Result<Vec<PresetItem>, String> {
    let path = get_presets_path(&app_handle)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_presets(presets: Vec<PresetItem>, app_handle: AppHandle) -> Result<(), String> {
    let path = get_presets_path(&app_handle)?;
    let json_string = serde_json::to_string_pretty(&presets).map_err(|e| e.to_string())?;
    fs::write(path, json_string).map_err(|e| e.to_string())
}

fn get_internal_library_root_path(app_handle: &AppHandle) -> Result<std::path::PathBuf, String> {
    #[cfg(not(target_os = "android"))]
    {
        // BLITZRAW: one data directory, chosen and proved. See data_dir.rs.
        let library_dir = crate::data_dir::data_path(app_handle, "library");

        if !library_dir.exists() {
            fs::create_dir_all(&library_dir).map_err(|e| e.to_string())?;
        }
        Ok(library_dir)
    }
    #[cfg(target_os = "android")]
    {
        crate::android_integration::get_android_internal_library_root()
    }
}

#[tauri::command]
pub fn get_or_create_internal_library_root(app_handle: AppHandle) -> Result<String, String> {
    let library_root = get_internal_library_root_path(&app_handle)?;

    Ok(library_root.to_string_lossy().to_string())
}

fn preset_file_display_name(file_path: &str) -> String {
    Path::new(file_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| file_path.to_string())
}

fn collect_top_level_preset_names(items: &[PresetItem]) -> HashSet<String> {
    items
        .iter()
        .map(|item| match item {
            PresetItem::Preset(p) => p.name.clone(),
            PresetItem::Folder(f) => f.name.clone(),
        })
        .collect()
}

fn parse_preset_file(file_path: &str) -> Result<Vec<PresetItem>, String> {
    let lower_path = file_path.to_lowercase();
    let is_legacy = lower_path.ends_with(".xmp") || lower_path.ends_with(".lrtemplate");

    if !is_legacy {
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read preset file: {}", e))?;
        let preset_file: PresetFile = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse preset file: {}", e))?;
        return Ok(preset_file.presets);
    }

    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read legacy preset file: {}", e))?;

    let xmp_content = if lower_path.ends_with(".lrtemplate") {
        let re = Regex::new(r#"(?s)s.xmp = "(.*)""#)
            .map_err(|e| format!("Regex compilation failed: {}", e))?;
        if let Some(caps) = re.captures(&content) {
            caps.get(1)
                .map(|m| m.as_str().replace(r#"\""#, r#"""#))
                .unwrap_or(content)
        } else {
            content
        }
    } else {
        content
    };

    let converted_preset = preset_converter::convert_xmp_to_preset(&xmp_content)?;
    Ok(vec![PresetItem::Preset(converted_preset)])
}

fn merge_imported_items(
    target: &mut Vec<PresetItem>,
    taken_names: &mut HashSet<String>,
    imported: Vec<PresetItem>,
) {
    for mut imported_item in imported {
        let original_name = match &mut imported_item {
            PresetItem::Preset(p) => {
                p.id = Uuid::new_v4().to_string();
                p.name.clone()
            }
            PresetItem::Folder(f) => {
                f.id = Uuid::new_v4().to_string();
                for child in &mut f.children {
                    child.id = Uuid::new_v4().to_string();
                }
                f.name.clone()
            }
        };

        let mut new_name = original_name.clone();
        let mut counter = 1;
        while taken_names.contains(&new_name) {
            new_name = format!("{} ({})", original_name, counter);
            counter += 1;
        }

        match &mut imported_item {
            PresetItem::Preset(p) => p.name = new_name.clone(),
            PresetItem::Folder(f) => f.name = new_name.clone(),
        }

        taken_names.insert(new_name);
        target.push(imported_item);
    }
}

fn import_preset_file_into_library(
    file_path: &str,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    let imported = parse_preset_file(file_path)?;

    let mut current_presets = load_presets(app_handle.clone())?;
    let mut taken_names = collect_top_level_preset_names(&current_presets);
    merge_imported_items(&mut current_presets, &mut taken_names, imported);

    save_presets(current_presets.clone(), app_handle)?;
    Ok(current_presets)
}

#[tauri::command]
pub fn handle_import_presets_from_file(
    file_path: String,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    import_preset_file_into_library(&file_path, app_handle)
}

#[tauri::command]
pub fn handle_import_legacy_presets_from_file(
    file_path: String,
    app_handle: AppHandle,
) -> Result<Vec<PresetItem>, String> {
    import_preset_file_into_library(&file_path, app_handle)
}

#[tauri::command]
pub fn handle_import_presets_from_files(
    file_paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<PresetImportResult, String> {
    let mut current_presets = load_presets(app_handle.clone())?;
    let mut taken_names = collect_top_level_preset_names(&current_presets);

    let mut failures: Vec<PresetImportFailure> = Vec::new();
    let mut library_changed = false;

    for file_path in &file_paths {
        match parse_preset_file(file_path) {
            Ok(imported) => {
                library_changed |= !imported.is_empty();
                merge_imported_items(&mut current_presets, &mut taken_names, imported);
            }
            Err(error) => failures.push(PresetImportFailure {
                file_name: preset_file_display_name(file_path),
                error,
            }),
        }
    }

    if library_changed {
        save_presets(current_presets.clone(), app_handle)?;
    }

    Ok(PresetImportResult {
        presets: current_presets,
        failures,
    })
}

#[tauri::command]
pub fn handle_export_presets_to_file(
    presets_to_export: Vec<PresetItem>,
    file_path: String,
) -> Result<(), String> {
    let preset_file = ExportPresetFile {
        creator: "Anonymous",
        presets: &presets_to_export,
    };

    let json_string = serde_json::to_string_pretty(&preset_file)
        .map_err(|e| format!("Failed to serialize presets: {}", e))?;
    fs::write(file_path, json_string).map_err(|e| format!("Failed to write preset file: {}", e))
}

#[tauri::command]
pub fn save_community_preset(
    name: String,
    adjustments: Value,
    app_handle: AppHandle,
    include_masks: Option<bool>,
    include_crop_transform: Option<bool>,
    preset_type: Option<String>,
) -> Result<(), String> {
    let mut current_presets = load_presets(app_handle.clone())?;

    let community_folder_name = "Community";
    let community_folder_id = match current_presets.iter_mut().find(|item| {
        if let PresetItem::Folder(f) = item {
            f.name == community_folder_name
        } else {
            false
        }
    }) {
        Some(PresetItem::Folder(folder)) => folder.id.clone(),
        _ => {
            let new_folder_id = Uuid::new_v4().to_string();
            let new_folder = PresetItem::Folder(PresetFolder {
                id: new_folder_id.clone(),
                name: community_folder_name.to_string(),
                children: Vec::new(),
            });
            current_presets.insert(0, new_folder);
            new_folder_id
        }
    };

    let new_preset = Preset {
        id: Uuid::new_v4().to_string(),
        name,
        adjustments,
        include_masks,
        include_crop_transform,
        preset_type: preset_type.or(Some("style".to_string())),
    };

    if let Some(PresetItem::Folder(folder)) = current_presets.iter_mut().find(|item| {
        if let PresetItem::Folder(f) = item {
            f.id == community_folder_id
        } else {
            false
        }
    }) {
        folder.children.retain(|p| p.name != new_preset.name);
        folder.children.push(new_preset);
    }

    save_presets(current_presets, app_handle)
}

#[tauri::command]
pub fn clear_all_sidecars(root_path: String) -> Result<usize, String> {
    if !Path::new(&root_path).exists() {
        return Err(format!("Root path does not exist: {}", root_path));
    }

    // ============ BLITZRAW: to the Recycle Bin, like everything else ============
    // This was the one delete in the app that went straight to permanent. It
    // takes every edit on every photo under every root folder, which here is
    // two whole drives, and one click was all of it gone for good.
    //
    // Collected first and sent in one call, so a run that fails part way
    // through has not already destroyed half the library.
    let mut to_trash = Vec::new();
    for entry in WalkDir::new(root_path).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file()
            && let Some(extension) = path.extension()
            && (extension == "rrdata" || extension == "rrexif")
        {
            to_trash.push(path.to_path_buf());
        }
    }

    let deleted_count = to_trash.len();
    if deleted_count == 0 {
        return Ok(0);
    }

    log::info!("Clearing {deleted_count} sidecar files to the Recycle Bin");
    if let Err(trash_error) = trash::delete_all(&to_trash) {
        return Err(format!(
            "Could not move the sidecar files to the Recycle Bin, so none were deleted: {trash_error}"
        ));
    }

    Ok(deleted_count)
    // ========== BLITZRAW END: to the Recycle Bin, like everything else ==========
}

#[tauri::command]
pub fn clear_thumbnail_cache(app_handle: AppHandle) -> Result<(), String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");

    if thumb_cache_dir.exists() {
        fs::remove_dir_all(&thumb_cache_dir)
            .map_err(|e| format!("Failed to remove thumbnail cache: {}", e))?;
    }

    fs::create_dir_all(&thumb_cache_dir)
        .map_err(|e| format!("Failed to recreate thumbnail cache directory: {}", e))?;

    Ok(())
}

#[tauri::command]
pub fn show_in_finder(path: String) -> Result<(), String> {
    let (source_path, _) = parse_virtual_path(&path);

    #[cfg(target_os = "windows")]
    {
        let source_path_str = source_path.to_string_lossy().to_string();
        Command::new("explorer")
            .args(["/select,", &source_path_str])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "macos")]
    {
        let source_path_str = source_path.to_string_lossy().to_string();
        Command::new("open")
            .args(["-R", &source_path_str])
            .spawn()
            .map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(parent) = source_path.parent() {
            Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map_err(|e| e.to_string())?;
        } else {
            return Err("Could not get parent directory".into());
        }
    }

    #[cfg(target_os = "android")]
    {
        return Err("Show in File Manager is not natively supported via CLI on Android.".into());
    }

    #[cfg(target_os = "ios")]
    {
        return Err("Show in File Manager is not supported on iOS.".into());
    }

    Ok(())
}

#[tauri::command]
pub fn delete_files_from_disk(paths: Vec<String>, app_handle: AppHandle) -> Result<(), String> {
    let mut files_to_trash = HashSet::new();

    let mut deletions = HashSet::new();

    for path_str in paths {
        let (source_path, sidecar_path) = parse_virtual_path(&path_str);
        deletions.insert(path_str.clone());

        if path_str.contains("?vc=") {
            if sidecar_path.exists() {
                files_to_trash.insert(sidecar_path);
            }
        } else {
            if source_path.exists() {
                match find_all_associated_files(&source_path) {
                    Ok(associated_files) => {
                        for file in associated_files {
                            files_to_trash.insert(file);
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "Could not find associated files for {}: {}",
                            source_path.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    if files_to_trash.is_empty() {
        return Ok(());
    }

    let final_paths_to_delete: Vec<PathBuf> = files_to_trash.into_iter().collect();
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    if let Err(trash_error) = trash::delete_all(&final_paths_to_delete) {
        log::warn!(
            "Failed to move files to trash: {}. Falling back to permanent delete.",
            trash_error
        );
        for path in final_paths_to_delete {
            if path.is_file() {
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete file {}: {}", path.display(), e))?;
            } else if path.is_dir() {
                fs::remove_dir_all(&path)
                    .map_err(|e| format!("Failed to delete directory {}: {}", path.display(), e))?;
            }
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    for path in final_paths_to_delete {
        if path.is_file() {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to delete file {}: {}", path.display(), e))?;
        } else if path.is_dir() {
            fs::remove_dir_all(&path)
                .map_err(|e| format!("Failed to delete directory {}: {}", path.display(), e))?;
        }
    }

    sync_album_path_changes(&app_handle, None, Some(&deletions), None);

    Ok(())
}

fn deletion_stem_for(filename: &str) -> Option<&str> {
    let image_filename = if filename.ends_with(".rrdata") {
        let without_rrdata = filename.trim_end_matches(".rrdata");
        if let Some(dot_pos) = without_rrdata.rfind('.') {
            let suffix = &without_rrdata[dot_pos + 1..];
            if suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_hexdigit()) {
                &without_rrdata[..dot_pos]
            } else {
                without_rrdata
            }
        } else {
            without_rrdata
        }
    } else if filename.ends_with(".rrexif") {
        filename.trim_end_matches(".rrexif")
    } else if is_supported_image_file(filename) {
        filename
    } else {
        return None;
    };
    Path::new(image_filename)
        .file_stem()
        .and_then(|s| s.to_str())
}

#[tauri::command]
pub fn delete_files_with_associated(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut stems_to_delete = HashSet::new();
    let mut parent_dirs = HashSet::new();
    let mut deletions = HashSet::new();

    for path_str in &paths {
        deletions.insert(path_str.clone());
        let (source_path, _) = parse_virtual_path(path_str);
        if let Some(stem) = source_path.file_stem().and_then(|s| s.to_str()) {
            stems_to_delete.insert(stem.to_string());
        }
        if let Some(parent) = source_path.parent() {
            parent_dirs.insert(parent.to_path_buf());
        }
    }

    if stems_to_delete.is_empty() {
        return Ok(());
    }

    let mut files_to_trash = HashSet::new();

    for parent_dir in parent_dirs {
        if let Ok(entries) = fs::read_dir(parent_dir) {
            for entry in entries.filter_map(Result::ok) {
                let entry_path = entry.path();
                if !entry_path.is_file() {
                    continue;
                }

                let entry_filename = entry.file_name();
                let entry_filename_str = entry_filename.to_string_lossy();

                if let Some(stem) = deletion_stem_for(&entry_filename_str)
                    && stems_to_delete.contains(stem)
                {
                    files_to_trash.insert(entry_path);
                }
            }
        }
    }

    if files_to_trash.is_empty() {
        return Ok(());
    }

    let final_paths_to_delete: Vec<PathBuf> = files_to_trash.into_iter().collect();
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    if let Err(trash_error) = trash::delete_all(&final_paths_to_delete) {
        log::warn!(
            "Failed to move files to trash: {}. Falling back to permanent delete.",
            trash_error
        );
        for path in final_paths_to_delete {
            if path.is_file() {
                fs::remove_file(&path)
                    .map_err(|e| format!("Failed to delete file {}: {}", path.display(), e))?;
            }
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    for path in final_paths_to_delete {
        if path.is_file() {
            fs::remove_file(&path)
                .map_err(|e| format!("Failed to delete file {}: {}", path.display(), e))?;
        }
    }

    sync_album_path_changes(&app_handle, None, Some(&deletions), None);

    Ok(())
}

pub fn get_thumb_cache_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app_handle
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?;
    let thumb_cache_dir = cache_dir.join("thumbnails");
    if !thumb_cache_dir.exists() {
        fs::create_dir_all(&thumb_cache_dir).map_err(|e| e.to_string())?;
    }
    Ok(thumb_cache_dir)
}

// ============= BLITZRAW: a thumbnail from a picture already in hand =============
/// Writes an already-rendered picture into the thumbnail cache.
///
/// The counterpart of `preview_cache::store_rendered_preview`, split out for
/// the same reason: something that has just rendered this photo should not have
/// to decode it again to leave a thumbnail behind. A merge is the case that
/// wanted it, since the merged picture is in memory at the moment the file is
/// written and both caches are about to be asked for it.
///
/// Keyed exactly as the normal path keys it, so what is written here is found
/// by an ordinary cache lookup rather than by anything knowing a merge made it.
pub fn store_rendered_thumbnail(
    path_str: &str,
    rendered: &DynamicImage,
    app_handle: &AppHandle,
) -> std::result::Result<PathBuf, String> {
    let dir = get_thumb_cache_dir(app_handle)?;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let width = settings.thumbnail_resolution.unwrap_or(720);

    let data = encode_thumbnail(rendered, width).map_err(|e| e.to_string())?;
    let target = thumbnail_path_for(&dir, path_str);
    write_thumbnail(&target, &data).map_err(|e| format!("{}: {e}", target.display()))?;
    // BLITZRAW: a merge writes its own thumbnail from pixels it already has,
    // and it needs a key like any other or the next scan re-renders it.
    let (_, sidecar) = parse_virtual_path(path_str);
    let adjustments = crate::exif_processing::load_sidecar(&sidecar).adjustments;
    if let Some(key) = thumbnail_freshness_key(path_str, &adjustments, &settings) {
        let _ = fs::write(thumbnail_key_path(&target), key);
    }
    Ok(target)
}
// =========== BLITZRAW END: a thumbnail from a picture already in hand ===========

pub fn get_cached_or_generate_thumbnail_image(
    path_str: &str,
    app_handle: &AppHandle,
    gpu_context: Option<&GpuContext>,
) -> Result<DynamicImage> {
    let thumb_cache_dir = get_thumb_cache_dir(app_handle).map_err(|e| anyhow::anyhow!(e))?;
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let target_width = settings.thumbnail_resolution.unwrap_or(720);

    {
        let cache_path = thumbnail_path_for(&thumb_cache_dir, path_str);

        let adjustments = {
            let (_, sidecar) = parse_virtual_path(path_str);
            crate::exif_processing::load_sidecar(&sidecar).adjustments
        };
        if cache_path.exists()
            && thumbnail_is_current(&cache_path, path_str, &adjustments, &settings)
        {
            if let Ok(image) = image::open(&cache_path) {
                return Ok(image);
            }
            eprintln!(
                "Could not open cached thumbnail, regenerating: {:?}",
                cache_path
            );
        }

        let thumb_image = generate_thumbnail_data(path_str, gpu_context, None, app_handle, None)?;
        let thumb_data = encode_thumbnail(&thumb_image, target_width)?;
        write_thumbnail(&cache_path, &thumb_data)?;

        Ok(thumb_image)
    }
}

#[tauri::command]
pub async fn import_files(
    source_paths: Vec<String>,
    destination_folder: String,
    settings: ImportSettings,
    app_handle: AppHandle,
) -> Result<(), String> {
    let total_files = source_paths.len();
    let _ = app_handle.emit("import-start", serde_json::json!({ "total": total_files }));

    tauri::async_runtime::spawn_blocking(move || {
        for (i, source_path_str) in source_paths.iter().enumerate() {
            let _ = app_handle.emit(
                "import-progress",
                serde_json::json!({ "current": i, "total": total_files, "path": source_path_str }),
            );

            let import_result: Result<(), String> = (|| {
                #[cfg(target_os = "android")]
                if is_android_content_uri(source_path_str) {
                    let resolved_name = resolve_android_content_uri_name(source_path_str)?;
                    let source_bytes = read_android_content_uri(source_path_str)?;
                    let source_name_path = Path::new(&resolved_name);
                    let file_date = exif_processing::get_creation_date_from_bytes(
                        &resolved_name,
                        &source_bytes,
                    );

                    let mut final_dest_folder = PathBuf::from(&destination_folder);
                    if settings.organize_by_date {
                        let date_format_str = settings
                            .date_folder_format
                            .replace("YYYY", "%Y")
                            .replace("MM", "%m")
                            .replace("DD", "%d");
                        let subfolder = file_date.format(&date_format_str).to_string();
                        final_dest_folder.push(subfolder);
                    }

                    fs::create_dir_all(&final_dest_folder)
                        .map_err(|e| format!("Failed to create destination folder: {}", e))?;

                    let new_stem = generate_filename_from_template(
                        &settings.filename_template,
                        source_name_path,
                        i + 1,
                        total_files,
                        &file_date,
                    );
                    let extension = source_name_path
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    let new_filename = format!("{}.{}", new_stem, extension);
                    let dest_file_path = final_dest_folder.join(new_filename);

                    if dest_file_path.exists() {
                        return Err(format!(
                            "File already exists at destination: {}",
                            dest_file_path.display()
                        ));
                    }

                    fs::write(&dest_file_path, source_bytes).map_err(|e| e.to_string())?;

                    if settings.delete_after_import {
                        log::info!(
                            "Skipping delete_after_import for Android content URI source: {}",
                            source_path_str
                        );
                    }

                    return Ok(());
                }

                let (source_path, source_sidecar) = parse_virtual_path(source_path_str);
                if !source_path.exists() {
                    return Err(format!("Source file not found: {}", source_path_str));
                }

                let file_date = exif_processing::get_creation_date_from_path(&source_path);

                let mut final_dest_folder = PathBuf::from(&destination_folder);
                if settings.organize_by_date {
                    let date_format_str = settings
                        .date_folder_format
                        .replace("YYYY", "%Y")
                        .replace("MM", "%m")
                        .replace("DD", "%d");
                    let subfolder = file_date.format(&date_format_str).to_string();
                    final_dest_folder.push(subfolder);
                }

                fs::create_dir_all(&final_dest_folder)
                    .map_err(|e| format!("Failed to create destination folder: {}", e))?;

                let new_stem = generate_filename_from_template(
                    &settings.filename_template,
                    &source_path,
                    i + 1,
                    total_files,
                    &file_date,
                );
                let extension = source_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let new_filename = format!("{}.{}", new_stem, extension);
                let dest_file_path = final_dest_folder.join(new_filename);

                if dest_file_path.exists() {
                    return Err(format!(
                        "File already exists at destination: {}",
                        dest_file_path.display()
                    ));
                }

                fs::copy(&source_path, &dest_file_path).map_err(|e| e.to_string())?;
                if source_sidecar.exists()
                    && let Some(dest_str) = dest_file_path.to_str()
                {
                    let (_, dest_sidecar) = parse_virtual_path(dest_str);
                    fs::copy(&source_sidecar, &dest_sidecar).map_err(|e| e.to_string())?;
                }

                let mut source_rrexif_name = source_path.file_name().unwrap().to_os_string();
                source_rrexif_name.push(".rrexif");
                let source_rrexif = source_path.with_file_name(source_rrexif_name);

                if source_rrexif.exists() {
                    let mut dest_rrexif_name = dest_file_path.file_name().unwrap().to_os_string();
                    dest_rrexif_name.push(".rrexif");
                    let dest_rrexif = dest_file_path.with_file_name(dest_rrexif_name);
                    let _ = fs::copy(&source_rrexif, &dest_rrexif);
                }

                if settings.delete_after_import {
                    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
                    {
                        if let Err(trash_error) = trash::delete(&source_path) {
                            log::warn!(
                                "Failed to trash source file {}: {}. Deleting permanently.",
                                source_path.display(),
                                trash_error
                            );
                            fs::remove_file(&source_path).map_err(|e| e.to_string())?;
                        }
                        if source_sidecar.exists()
                            && let Err(trash_error) = trash::delete(&source_sidecar)
                        {
                            log::warn!(
                                "Failed to trash source sidecar {}: {}. Deleting permanently.",
                                source_sidecar.display(),
                                trash_error
                            );
                            fs::remove_file(&source_sidecar).map_err(|e| e.to_string())?;
                        }
                    }

                    #[cfg(not(any(
                        target_os = "windows",
                        target_os = "macos",
                        target_os = "linux"
                    )))]
                    {
                        fs::remove_file(&source_path).map_err(|e| e.to_string())?;
                        if source_sidecar.exists() {
                            fs::remove_file(&source_sidecar).map_err(|e| e.to_string())?;
                        }
                        if source_rrexif.exists() {
                            let _ = fs::remove_file(&source_rrexif);
                        }
                    }
                }

                Ok(())
            })();

            if let Err(e) = import_result {
                eprintln!("Failed to import {}: {}", source_path_str, e);
                let _ = app_handle.emit("import-error", e);
                return;
            }
        }

        let _ = app_handle.emit(
            "import-progress",
            serde_json::json!({ "current": total_files, "total": total_files, "path": "" }),
        );
        let _ = app_handle.emit("import-complete", ());
    });

    Ok(())
}

pub fn generate_filename_from_template(
    template: &str,
    original_path: &std::path::Path,
    sequence: usize,
    total: usize,
    file_date: &DateTime<Utc>,
) -> String {
    let stem = original_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let sequence_str = format!(
        "{:0width$}",
        sequence,
        width = total.to_string().len().max(1)
    );
    let local_date = file_date.with_timezone(&chrono::Local);

    let mut result = template.to_string();
    result = result.replace("{original_filename}", stem);
    result = result.replace("{sequence}", &sequence_str);
    result = result.replace("{YYYY}", &local_date.format("%Y").to_string());
    result = result.replace("{MM}", &local_date.format("%m").to_string());
    result = result.replace("{DD}", &local_date.format("%d").to_string());
    result = result.replace("{hh}", &local_date.format("%H").to_string());
    result = result.replace("{mm}", &local_date.format("%M").to_string());

    result
}

#[tauri::command]
pub fn rename_files(
    paths: Vec<String>,
    name_template: String,
    app_handle: AppHandle,
) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut operations: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut final_new_paths = Vec::with_capacity(paths.len());
    let mut renames = HashMap::new();

    for (i, path_str) in paths.iter().enumerate() {
        let (original_path, _) = parse_virtual_path(path_str);
        if !original_path.exists() {
            return Err(format!("File not found: {}", path_str));
        }

        let parent = original_path
            .parent()
            .ok_or("Could not get parent directory")?;
        let extension = original_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let file_date = exif_processing::get_creation_date_from_path(&original_path);

        let new_stem = generate_filename_from_template(
            &name_template,
            &original_path,
            i + 1,
            paths.len(),
            &file_date,
        );
        let new_filename = format!("{}.{}", new_stem, extension);
        let new_path = parent.join(new_filename);

        if new_path.exists() && new_path != original_path {
            return Err(format!(
                "A file with the name {} already exists.",
                new_path.display()
            ));
        }

        operations.insert(original_path, new_path);
    }

    let mut sidecar_operations: HashMap<PathBuf, PathBuf> = HashMap::new();
    for (original_path, new_path) in &operations {
        let parent = original_path
            .parent()
            .ok_or("Could not get parent directory")?;
        let original_filename_str = original_path.file_name().unwrap().to_string_lossy();
        let new_filename_str = new_path.file_name().unwrap().to_string_lossy();

        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.filter_map(Result::ok) {
                let entry_path = entry.path();
                let entry_os_filename = entry.file_name();
                let entry_filename = entry_os_filename.to_string_lossy();

                if entry_filename.starts_with(&format!("{}.", original_filename_str))
                    && entry_filename.ends_with(".rrdata")
                {
                    let new_sidecar_filename =
                        entry_filename.replacen(&*original_filename_str, &new_filename_str, 1);
                    let new_sidecar_path = parent.join(new_sidecar_filename);
                    sidecar_operations.insert(entry_path, new_sidecar_path);
                } else if entry_filename == format!("{}.rrdata", original_filename_str) {
                    let mut new_sidecar_name = new_path.file_name().unwrap().to_os_string();
                    new_sidecar_name.push(".rrdata");
                    let new_sidecar_path = new_path.with_file_name(new_sidecar_name);

                    sidecar_operations.insert(entry_path, new_sidecar_path);
                }
            }
        }

        let mut old_rrexif_name = original_path.file_name().unwrap().to_os_string();
        old_rrexif_name.push(".rrexif");
        let old_rrexif = original_path.with_file_name(old_rrexif_name);

        if old_rrexif.exists() {
            let mut new_rrexif_name = new_path.file_name().unwrap().to_os_string();
            new_rrexif_name.push(".rrexif");
            let new_rrexif = new_path.with_file_name(new_rrexif_name);
            sidecar_operations.insert(old_rrexif, new_rrexif);
        }
    }
    operations.extend(sidecar_operations);

    for (old_path, new_path) in operations {
        fs::rename(&old_path, &new_path).map_err(|e| {
            format!(
                "Failed to rename {} to {}: {}",
                old_path.display(),
                new_path.display(),
                e
            )
        })?;

        let old_str = old_path.to_string_lossy().into_owned();
        let new_str = new_path.to_string_lossy().into_owned();

        renames.insert(old_str, new_str.clone());

        if is_supported_image_file(&new_path) {
            final_new_paths.push(new_str);
        }
    }

    // Stack membership is stored by file name, so it has to follow the rename
    // or the stack loses the frame. Renaming a whole peer stack, which is what
    // the selection rules now do, would otherwise destroy it outright.
    let touched = crate::stacks::rename_members(&renames);
    if touched > 0 {
        log::info!("Renamed stack members across {touched} folder(s)");
    }

    sync_album_path_changes(&app_handle, Some(&renames), None, None);

    Ok(final_new_paths)
}

#[tauri::command]
pub fn create_virtual_copy(
    source_virtual_path: String,
    target_album_id: Option<String>,
    app_handle: AppHandle,
) -> Result<String, String> {
    let (source_path, source_sidecar_path) = parse_virtual_path(&source_virtual_path);

    let new_copy_id = Uuid::new_v4().to_string()[..6].to_string();
    let new_virtual_path = format!("{}?vc={}", source_path.to_string_lossy(), new_copy_id);
    let (_, new_sidecar_path) = parse_virtual_path(&new_virtual_path);

    if source_sidecar_path.exists() {
        fs::copy(&source_sidecar_path, &new_sidecar_path)
            .map_err(|e| format!("Failed to copy sidecar file: {}", e))?;
    } else {
        let default_metadata = ImageMetadata::default();
        let json_string =
            serde_json::to_string_pretty(&default_metadata).map_err(|e| e.to_string())?;
        fs::write(new_sidecar_path, json_string).map_err(|e| e.to_string())?;
    }

    if let Some(album_id) = target_album_id {
        let _ = add_to_album(album_id, vec![new_virtual_path.clone()], app_handle);
    }

    Ok(new_virtual_path)
}

pub fn extract_xmp_rating(content: &str) -> Option<u8> {
    if let Some(idx) = content.find("xmp:Rating=\"") {
        let start = idx + 12;
        let end = content[start..].find('"').map(|i| start + i)?;
        return content[start..end].parse().ok();
    }
    if let Some(idx) = content.find("<xmp:Rating>") {
        let start = idx + 12;
        let end = content[start..].find('<').map(|i| start + i)?;
        return content[start..end].parse().ok();
    }
    None
}

pub fn extract_xmp_label(content: &str) -> Option<String> {
    if let Some(idx) = content.find("xmp:Label=\"") {
        let start = idx + 11;
        let end = content[start..].find('"').map(|i| start + i)?;
        return Some(content[start..end].to_string());
    }
    if let Some(idx) = content.find("<xmp:Label>") {
        let start = idx + 11;
        let end = content[start..].find('<').map(|i| start + i)?;
        return Some(content[start..end].to_string());
    }
    None
}

pub fn extract_xmp_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(start_idx) = content.find("<dc:subject>")
        && let Some(end_idx) = content[start_idx..].find("</dc:subject>")
    {
        let subject_block = &content[start_idx..start_idx + end_idx];
        let mut current_idx = 0;
        while let Some(li_start) = subject_block[current_idx..].find("<rdf:li>") {
            let val_start = current_idx + li_start + 8;
            if let Some(li_end) = subject_block[val_start..].find("</rdf:li>") {
                tags.push(subject_block[val_start..val_start + li_end].to_string());
                current_idx = val_start + li_end + 9;
            } else {
                break;
            }
        }
    }
    tags
}

pub fn resolve_xmp_path(image_path: &Path) -> Option<PathBuf> {
    let xmp_path = image_path.with_extension("xmp");
    let xmp_path_upper = image_path.with_extension("XMP");
    if xmp_path.exists() {
        Some(xmp_path)
    } else if xmp_path_upper.exists() {
        Some(xmp_path_upper)
    } else {
        None
    }
}

pub fn sync_metadata_from_xmp(source_path: &Path, metadata: &mut ImageMetadata) -> bool {
    let actual_xmp = resolve_xmp_path(source_path);

    if let Some(xmp_file) = actual_xmp
        && let Ok(content) = fs::read_to_string(&xmp_file)
    {
        return apply_xmp_content(&content, metadata);
    }
    false
}

// ===================== BLITZRAW: in-camera ratings =====================
// Stars added on the camera during a shoot live in an XMP packet inside the
// photo, not in a `.xmp` sidecar, which is why Lightroom, Bridge and Explorer
// all showed them and we did not. See `embedded_xmp.rs`.
//
// Consulted only for a photo that has no `.rrdata` sidecar yet. Once BlitzRaw
// has written one the sidecar is the truth, so clearing a star here is not
// undone by the camera on the next scan.
pub fn sync_metadata_from_embedded_xmp(source_path: &Path, metadata: &mut ImageMetadata) -> bool {
    // Asked once and never again. Every photo here already had a sidecar before
    // this existed, written to cache its EXIF, so "has no sidecar" was not a
    // usable stand-in for "has never been asked".
    if metadata.camera_rating.is_some() {
        return false;
    }

    // Reading a placeholder would pull the whole file down from the cloud for
    // the sake of one number.
    if is_cloud_placeholder(source_path) {
        return false;
    }

    let embedded = crate::embedded_xmp::read(source_path);

    // A body that writes only the Explorer tag and no packet still gets read.
    let found = embedded
        .xmp
        .as_deref()
        .and_then(extract_xmp_rating)
        .or(embedded.exif_rating)
        .unwrap_or(0);

    metadata.camera_rating = Some(found);

    if let Some(content) = embedded.xmp.as_deref() {
        apply_xmp_content(content, metadata);
    } else {
        apply_xmp_rating(found, metadata);
    }

    // Always a change, because the answer is worth recording even when it is
    // zero. That write is what stops the next scan asking again.
    true
}
// =================== BLITZRAW END: in-camera ratings ===================

/// Takes the rating, colour label and keywords out of one XMP document and
/// folds them into `metadata`. Shared by the sidecar and the embedded packet,
/// which carry the same fields and differ only in where they are kept.
fn apply_xmp_content(content: &str, metadata: &mut ImageMetadata) -> bool {
    let mut changed = false;

    if let Some(rating) = extract_xmp_rating(content) {
        changed |= apply_xmp_rating(rating, metadata);
    }

    let xmp_label = extract_xmp_label(content);
    let xmp_tags = extract_xmp_tags(content);

    let mut current_tags = metadata.tags.clone().unwrap_or_default();
    let original_len = current_tags.len();
    let had_no_tags = metadata.tags.is_none();

    for tag in xmp_tags {
        if !current_tags.contains(&tag) {
            current_tags.push(tag);
        }
    }

    if let Some(label) = xmp_label {
        let label_tag = format!("{}{}", COLOR_TAG_PREFIX, label.to_lowercase());
        if !current_tags.contains(&label_tag) {
            current_tags.retain(|t| !t.starts_with(COLOR_TAG_PREFIX));
            current_tags.push(label_tag);
        }
    }

    if current_tags.len() != original_len || (had_no_tags && !current_tags.is_empty()) {
        metadata.tags = Some(current_tags);
        changed = true;
    }

    changed
}

/// A rating from outside never overwrites one set in BlitzRaw, and zero means
/// "no opinion" rather than "unrated", so neither can clear a star.
fn apply_xmp_rating(rating: u8, metadata: &mut ImageMetadata) -> bool {
    if rating == 0 || metadata.rating != 0 {
        return false;
    }

    metadata.rating = rating;
    if let Some(obj) = metadata.adjustments.as_object_mut() {
        obj.insert("rating".to_string(), serde_json::json!(rating));
    } else {
        metadata.adjustments = serde_json::json!({"rating": rating});
    }
    true
}

pub fn sync_metadata_to_xmp(source_path: &Path, metadata: &ImageMetadata, create_if_missing: bool) {
    let xmp_path = source_path.with_extension("xmp");
    let xmp_path_upper = source_path.with_extension("XMP");

    let mut actual_xmp = if xmp_path.exists() {
        Some(xmp_path.clone())
    } else if xmp_path_upper.exists() {
        Some(xmp_path_upper.clone())
    } else {
        None
    };

    if actual_xmp.is_none() {
        if !create_if_missing {
            return;
        }
        let skeleton = r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="RapidRAW">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;
        if let Err(e) = fs::write(&xmp_path, skeleton) {
            log::error!("Failed to create skeleton XMP: {}", e);
            return;
        }
        actual_xmp = Some(xmp_path);
    }

    if let Some(xmp_file) = actual_xmp
        && let Ok(mut content) = fs::read_to_string(&xmp_file)
    {
        let rating_str = metadata.rating.to_string();
        let re_rating_attr = Regex::new(r#"xmp:Rating\s*=\s*"[^"]*""#).unwrap();
        let re_rating_tag = Regex::new(r#"<xmp:Rating\s*>[^<]*</xmp:Rating>"#).unwrap();

        if re_rating_attr.is_match(&content) {
            content = re_rating_attr
                .replace(&content, format!("xmp:Rating=\"{}\"", rating_str))
                .to_string();
        } else if re_rating_tag.is_match(&content) {
            content = re_rating_tag
                .replace(&content, format!("<xmp:Rating>{}</xmp:Rating>", rating_str))
                .to_string();
        } else if let Some(last_index) = content.rfind("</rdf:Description>") {
            let (start, end) = content.split_at(last_index);
            content = format!("{} <xmp:Rating>{}</xmp:Rating>\n{}", start, rating_str, end);
        }

        let current_tags = metadata.tags.clone().unwrap_or_default();
        let mut label = None;
        let mut normal_tags = Vec::new();

        for t in current_tags {
            if let Some(color) = t.strip_prefix(COLOR_TAG_PREFIX) {
                let mut c = color.chars();
                let cap_color = match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                };
                label = Some(cap_color);
            } else {
                normal_tags.push(t);
            }
        }

        if let Some(lbl) = label {
            let re_label_attr = Regex::new(r#"xmp:Label\s*=\s*"[^"]*""#).unwrap();
            let re_label_tag = Regex::new(r#"<xmp:Label\s*>[^<]*</xmp:Label>"#).unwrap();

            if re_label_attr.is_match(&content) {
                content = re_label_attr
                    .replace(&content, format!("xmp:Label=\"{}\"", lbl))
                    .to_string();
            } else if re_label_tag.is_match(&content) {
                content = re_label_tag
                    .replace(&content, format!("<xmp:Label>{}</xmp:Label>", lbl))
                    .to_string();
            } else if let Some(last_index) = content.rfind("</rdf:Description>") {
                let (start, end) = content.split_at(last_index);
                content = format!("{} <xmp:Label>{}</xmp:Label>\n{}", start, lbl, end);
            }
        } else {
            let re_label_attr = Regex::new(r#"\s*xmp:Label\s*=\s*"[^"]*""#).unwrap();
            let re_label_tag = Regex::new(r#"\s*<xmp:Label\s*>[^<]*</xmp:Label>"#).unwrap();
            content = re_label_attr.replace_all(&content, "").to_string();
            content = re_label_tag.replace_all(&content, "").to_string();
        }

        let re_subject =
            Regex::new(r#"(?s)<dc:subject>\s*<rdf:Bag>.*?</rdf:Bag>\s*</dc:subject>"#).unwrap();
        if normal_tags.is_empty() {
            content = re_subject.replace_all(&content, "").to_string();
        } else {
            let mut bag = String::from("<dc:subject>\n    <rdf:Bag>\n");
            for t in normal_tags {
                bag.push_str(&format!("     <rdf:li>{}</rdf:li>\n", t));
            }
            bag.push_str("    </rdf:Bag>\n   </dc:subject>");

            if re_subject.is_match(&content) {
                content = re_subject.replace(&content, bag).to_string();
            } else if let Some(last_index) = content.rfind("</rdf:Description>") {
                let (start, end) = content.split_at(last_index);
                content = format!("{} {}\n  {}", start, bag, end);
            }
        }

        let _ = fs::write(&xmp_file, content);
    }
}

// ===================== BLITZRAW: in-camera ratings =====================
#[cfg(test)]
mod camera_rating_tests {
    use super::*;
    use crate::image_processing::ImageMetadata;

    /// The question the old tests did not ask: does it survive the write?
    ///
    /// `records_an_unrated_photo_so_it_is_not_asked_twice` below proves the
    /// guard works on a struct held in memory, and says in its own comment that
    /// this "is what stops every scan rewriting every sidecar". It does not,
    /// because the half that matters is the round trip through the sidecar, and
    /// that half was not covered. A refactor then dropped `camera_rating` from
    /// the write and every test still passed, while every folder in the library
    /// re-rendered every thumbnail on every start-up, for ever.
    #[test]
    fn a_camera_rating_survives_being_written_and_read_back() {
        let dir = std::env::temp_dir().join("blitzraw-camera-rating-roundtrip");
        let _ = fs::create_dir_all(&dir);
        let sidecar = dir.join("photo.nef.rrdata");
        let _ = fs::remove_file(&sidecar);

        let mut synced = ImageMetadata::default();
        synced.rating = 0;
        synced.camera_rating = Some(3);

        write_synced_fields(&sidecar, &synced).expect("written");

        let read_back = crate::exif_processing::load_sidecar(&sidecar);
        assert_eq!(
            read_back.camera_rating,
            Some(3),
            "without this on disk, every scan asks the photo again and rewrites the sidecar"
        );
        assert!(
            !needs_camera_rating(&read_back),
            "and having asked once, it is never asked again"
        );
    }

    /// A scan that found nothing new must not touch the file. A write is a new
    /// modification time, and until the freshness key landed that alone threw
    /// away every cached picture of the photo.
    #[test]
    fn a_scan_that_changed_nothing_writes_nothing() {
        let dir = std::env::temp_dir().join("blitzraw-camera-rating-roundtrip");
        let _ = fs::create_dir_all(&dir);
        let sidecar = dir.join("untouched.nef.rrdata");
        let _ = fs::remove_file(&sidecar);

        let mut synced = ImageMetadata::default();
        synced.camera_rating = Some(0);
        write_synced_fields(&sidecar, &synced).expect("written");
        let first = fs::metadata(&sidecar).unwrap().modified().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        let written_again = write_synced_fields(&sidecar, &synced).expect("asked");

        assert!(written_again.is_none(), "nothing moved, so nothing was written");
        assert_eq!(first, fs::metadata(&sidecar).unwrap().modified().unwrap());
    }

    /// The smallest TIFF that carries an XMP packet with one rating in it.
    fn raw_rated(stars: u8) -> tempfile::NamedTempFile {
        let packet = format!(
            "<x:xmpmeta><rdf:Description><xmp:Rating>{stars}</xmp:Rating></rdf:Description></x:xmpmeta>"
        );
        let packet = packet.as_bytes();

        let mut buf = Vec::new();
        buf.extend_from_slice(b"II");
        buf.extend_from_slice(&42u16.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());

        let data_at: usize = 8 + 2 + 12 + 4;
        buf.extend_from_slice(&0x02bcu16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&(packet.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(data_at as u32).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(buf.len(), data_at);
        buf.extend_from_slice(packet);

        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), &buf).expect("write");
        file
    }

    #[test]
    fn takes_the_rating_the_camera_wrote() {
        let file = raw_rated(2);
        let mut metadata = ImageMetadata::default();

        assert!(sync_metadata_from_embedded_xmp(file.path(), &mut metadata));
        assert_eq!(metadata.rating, 2);
        assert_eq!(metadata.camera_rating, Some(2));
    }

    #[test]
    fn records_an_unrated_photo_so_it_is_not_asked_twice() {
        let file = raw_rated(0);
        let mut metadata = ImageMetadata::default();

        assert!(sync_metadata_from_embedded_xmp(file.path(), &mut metadata));
        assert_eq!(metadata.rating, 0);
        assert_eq!(metadata.camera_rating, Some(0));

        // Asked again, it says nothing, which is what stops every scan
        // rewriting every sidecar.
        assert!(!sync_metadata_from_embedded_xmp(file.path(), &mut metadata));
    }

    #[test]
    fn a_star_cleared_in_blitzraw_is_not_handed_back_by_the_camera() {
        let file = raw_rated(1);
        let mut metadata = ImageMetadata::default();
        sync_metadata_from_embedded_xmp(file.path(), &mut metadata);

        // What clearing the rating in the grid leaves behind.
        metadata.rating = 0;

        assert!(!sync_metadata_from_embedded_xmp(file.path(), &mut metadata));
        assert_eq!(metadata.rating, 0);
    }

    #[test]
    fn a_rating_set_in_blitzraw_wins_over_the_camera() {
        let file = raw_rated(1);
        let mut metadata = ImageMetadata::default();
        metadata.rating = 5;

        sync_metadata_from_embedded_xmp(file.path(), &mut metadata);

        assert_eq!(metadata.rating, 5);
        assert_eq!(metadata.camera_rating, Some(1));
    }

    #[test]
    fn a_file_with_nothing_to_say_is_still_only_asked_once() {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), b"not a photo at all").expect("write");

        let mut metadata = ImageMetadata::default();
        assert!(sync_metadata_from_embedded_xmp(file.path(), &mut metadata));
        assert_eq!(metadata.camera_rating, Some(0));
        assert!(!sync_metadata_from_embedded_xmp(file.path(), &mut metadata));
    }

    /// The rating rides in `adjustments` beside real edits, so importing one
    /// must not make an untouched photo claim it has been developed.
    #[test]
    fn an_imported_star_does_not_mark_the_photo_as_edited() {
        let file = raw_rated(1);
        let mut metadata = ImageMetadata::default();
        sync_metadata_from_embedded_xmp(file.path(), &mut metadata);

        assert!(!crate::image_processing::is_image_edited(
            &metadata.adjustments,
            true,
            None
        ));
    }

    #[test]
    fn the_rating_also_reaches_the_adjustments_the_editor_reads() {
        let file = raw_rated(3);
        let mut metadata = ImageMetadata::default();

        sync_metadata_from_embedded_xmp(file.path(), &mut metadata);

        assert_eq!(
            metadata.adjustments.get("rating").and_then(|v| v.as_u64()),
            Some(3)
        );
    }
}
// =================== BLITZRAW END: in-camera ratings ===================
