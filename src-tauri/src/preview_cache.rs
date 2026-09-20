//! Rendered previews on disk, beside the photographs they belong to.
//!
//! # Why
//!
//! Opening a photo large costs one full decode, and on a Z9 DNG that is 1.42
//! seconds and 545 MB of RAM. Everything after it is noise: downscaling to
//! 1920 takes 20ms and encoding it takes 51ms. So the pause before a sharp
//! picture appears is the decode, and the only way to remove it is to have
//! already done it. A rendered preview at the editor's own preview resolution
//! is 172 KB, which is 0.24 GB for a 1377 frame shoot.
//!
//! # What this is not
//!
//! A preview is not an editable image. It makes the photo appear at once, but
//! the first slider still waits for the decode, which the editor starts in the
//! background on open and usually finishes before anyone reaches for a
//! control. Lightroom draws the same line between a standard preview and a
//! smart one. The layout below leaves room for the second kind without moving
//! anything: previews live under a `preview` subfolder, so a `proxy` folder
//! can sit beside them with its own keys and its own discard.
//!
//! # Where
//!
//! `<folder of the photo>/.blitzraw-previews/preview/`, for the same reason
//! stacks live in `.blitzraw-stacks.json`: a shoot that moves drive or machine
//! takes its derivatives with it, and nothing has to know about a catalog that
//! does not exist yet. The folder is in `library_ignore`, or a recursive scan
//! would present the cache as if it were the library.
//!
//! Anything that copies a shoot elsewhere should skip this folder. It is
//! reproducible, it is not the photographer's work, and it would sync gigabytes
//! of it to cloud storage and backups for no reason.
//!
//! # Staleness
//!
//! The file name carries a hash of everything a preview depends on: which file
//! and which virtual copy, its modification time and length, the adjustments
//! that were applied, the width it was rendered at, and a format version. Miss
//! any of those and the cache lies. Modification time in particular: re-merging
//! an HDR writes the same path with different pixels, which is exactly how the
//! thumbnail store once kept showing a ghosted merge.
//!
//! A key that no longer matches is not found, so a stale preview is harmless
//! until something clears it out. Editing a photo rewrites its preview if it
//! already had one and deletes the previous key, so the cache stays honest for
//! files the user has chosen to keep warm without quietly building previews
//! for files they never asked about.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine;
use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::app_settings::load_settings;
use crate::app_state::AppState;
use crate::file_management::{generate_thumbnail_data, is_cloud_placeholder, parse_virtual_path};
use crate::gpu_processing;
use crate::legacy_names;

/// Bumped when a change makes previews written by an older build wrong rather
/// than merely different. Every key carries it, so the old ones stop matching.
///
/// 2: the base pre-sharpening stopped sharpening the noise, and noise
///    reduction briefly worked at three sizes rather than one.
/// 3: that three-size noise reduction was taken back out. Previews written by
///    2 show a filter the app no longer has, and nothing in the adjustments
///    would say so.
const FORMAT_VERSION: u32 = 3;

const CACHE_DIR_NAME: &str = ".blitzraw-previews";

/// What a cached derivative is. Only one kind exists today; the second is the
/// editable proxy described above, which would live in its own subfolder with
/// its own extension and share every other part of this module.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Preview,
}

impl PreviewKind {
    fn folder(self) -> &'static str {
        match self {
            PreviewKind::Preview => "preview",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            PreviewKind::Preview => "jpg",
        }
    }
}

/// Quality of the stored JPEG. Measured at 1920: q80 is 114 KB and q90 is 172
/// KB, and the difference is visible on a smooth sky, which real estate work is
/// full of. The extra 58 KB a frame is 80 MB across a whole shoot.
const JPEG_QUALITY: u8 = 90;

/// Falls back to the resolution the editor itself uses, since a preview that is
/// smaller than the editor's own render would be replaced by a sharper one the
/// moment the decode landed, which is the flicker this exists to remove.
fn preview_width(app_handle: &AppHandle) -> u32 {
    load_settings(app_handle.clone())
        .ok()
        .and_then(|s| s.editor_preview_resolution)
        .unwrap_or(1920)
        .clamp(320, 8192)
}

pub fn cache_dir_for(folder: &Path, kind: PreviewKind) -> PathBuf {
    // A shoot edited before the rename keeps its previews under the old folder
    // name. Renaming it here means the work already done is not thrown away.
    // If the rename cannot be done the previews are simply rendered again,
    // which costs time and nothing else, so no fallback is needed.
    legacy_names::migrate(folder, legacy_names::LEGACY_CACHE_DIR_NAME, CACHE_DIR_NAME);

    folder.join(CACHE_DIR_NAME).join(kind.folder())
}

/// The stem, kept in the file name so the folder can be read by a human and so
/// orphans can be found without opening anything. Sanitised because a virtual
/// copy carries a `?vc=` marker that is not legal in a file name.
fn file_stem_for(path_str: &str) -> String {
    let (source, _) = parse_virtual_path(path_str);
    let base = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let variant = path_str.split("?vc=").nth(1);
    let mut stem: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if let Some(v) = variant {
        stem.push_str("_vc");
        stem.push_str(
            &v.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>(),
        );
    }
    stem
}

// ============ BLITZRAW: a preview should survive the photo moving ============
/// What tells one photo in a folder from another.
///
/// # Why not the whole path
///
/// The path used to go into the cache key, and it made every preview useless
/// the moment a shoot moved. Copying a folder from the working drive to the
/// archive changes the path, so the key changes, so the app looks for a file it
/// never wrote while a perfectly good preview sits beside it. That is the whole
/// of the planned backup pipeline, and it would have rebuilt every preview at
/// each step of it.
///
/// The folder half of the path was never doing any work. A preview lives inside
/// the photo's own folder, so the folder is already known by where the file is.
/// What is left is the file's own name, which a filesystem guarantees is unique
/// within one folder, and the virtual copy id, which is the only way one path
/// can name two different pictures.
///
/// The name is used unsanitised on purpose. `file_stem_for` flattens anything
/// unusual to an underscore so the name can be written to disk, which would
/// make `a b.dng` and `a_b.dng` the same photo to this. They are two files.
///
/// The `?vc=` stays in front of the copy id so that a photo called `a` copy `1`
/// and a photo called `a1` cannot be confused.
fn identity_in_folder(path_str: &str) -> String {
    let (source, _) = parse_virtual_path(path_str);
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    match path_str.split("?vc=").nth(1) {
        Some(variant) => format!("{name}?vc={variant}"),
        None => name,
    }
}
// ========== BLITZRAW END: a preview should survive the photo moving ==========

/// Everything a preview depends on, hashed. Anything left out of here is a way
/// for the cache to show a picture that is no longer true.
fn cache_key(
    path_str: &str,
    adjustments_bytes: &[u8],
    width: u32,
    kind: PreviewKind,
) -> Option<String> {
    let (source, _) = parse_virtual_path(path_str);
    let meta = fs::metadata(&source).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();

    let mut hasher = blake3::Hasher::new();
    hasher.update(&FORMAT_VERSION.to_le_bytes());
    // BLITZRAW: which photo, said in a way that survives the photo moving.
    hasher.update(identity_in_folder(path_str).as_bytes());
    hasher.update(&modified.to_le_bytes());
    hasher.update(&meta.len().to_le_bytes());
    hasher.update(&width.to_le_bytes());
    hasher.update(kind.folder().as_bytes());
    hasher.update(adjustments_bytes);
    Some(hasher.finalize().to_hex()[..16].to_string())
}

fn adjustments_bytes_for(path_str: &str) -> Vec<u8> {
    let (_, sidecar) = parse_virtual_path(path_str);
    let metadata = crate::exif_processing::load_sidecar(&sidecar);
    serde_json::to_vec(&metadata.adjustments).unwrap_or_default()
}

/// Where this photo's preview would live, whether or not it is there yet.
fn preview_path_for(path_str: &str, width: u32, kind: PreviewKind) -> Option<PathBuf> {
    let (source, _) = parse_virtual_path(path_str);
    let folder = source.parent()?;
    let key = cache_key(path_str, &adjustments_bytes_for(path_str), width, kind)?;
    Some(cache_dir_for(folder, kind).join(format!(
        "{}__{}.{}",
        file_stem_for(path_str),
        key,
        kind.extension()
    )))
}

/// Every cached file for this photo, whatever key it was written under. Used to
/// clear the old one when an edit makes a new one, and to answer "does this
/// photo have a preview at all", which is what decides whether an edit is worth
/// re-rendering for.
fn existing_previews_for(path_str: &str, kind: PreviewKind) -> Vec<PathBuf> {
    let (source, _) = parse_virtual_path(path_str);
    let Some(folder) = source.parent() else {
        return Vec::new();
    };
    let dir = cache_dir_for(folder, kind);
    let prefix = format!("{}__", file_stem_for(path_str));
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&prefix))
                .unwrap_or(false)
        })
        .collect()
}

/// Encodes an already-rendered picture as this photo's preview, and clears
/// whatever key it used to be under.
///
/// Split out from the render so a caller that has just rendered the photo for
/// some other reason does not have to render it again. That is the whole of
/// keeping previews current: a thumbnail regeneration already pays the decode,
/// which is the expensive part, so rendering it at preview width instead and
/// taking the thumbnail from the same picture costs about thirty milliseconds
/// on top of a second and a half.
// ============ BLITZRAW: a picture of the photo, for anything that only needs one ============
/// The most recently written preview for a photo, at whatever width it was made.
///
/// For anything that wants a picture of how the photo looks now and does not
/// care that it is small. The scopes use it: a preview is a rendered JPEG with
/// the photo's own adjustments in it, so its histogram is the photo's histogram
/// to within the resampling, and it is on disk long before a raw decode
/// finishes.
pub fn newest_preview_for(path_str: &str) -> Option<PathBuf> {
    existing_previews_for(path_str, PreviewKind::Preview)
        .into_iter()
        .max_by_key(|candidate| fs::metadata(candidate).and_then(|m| m.modified()).ok())
}
// ========== BLITZRAW END: a picture of the photo, for anything that only needs one ==========

pub fn store_rendered_preview(
    path_str: &str,
    rendered: &image::DynamicImage,
    width: u32,
) -> Result<PathBuf, String> {
    let target = preview_path_for(path_str, width, PreviewKind::Preview)
        .ok_or_else(|| format!("Cannot place a preview for {path_str}"))?;
    let parent = target
        .parent()
        .ok_or_else(|| "Preview path has no folder".to_string())?;
    // A card, a read-only share or a locked folder cannot hold a preview. That
    // is not a failure worth stopping anything for, so it is reported per file
    // and the photo simply keeps decoding the way it always did.
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;

    let scaled = crate::image_processing::downscale_f32_image(rendered, width, width);
    let mut buf = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, JPEG_QUALITY)
        .encode_image(&scaled.to_rgb8())
        .map_err(|e| format!("{path_str}: {e}"))?;

    // Written beside and renamed, so a build interrupted part way through never
    // leaves a half-written file that would later be served as a preview.
    let temp = target.with_extension("jpg.part");
    fs::write(&temp, buf.into_inner()).map_err(|e| format!("{}: {e}", temp.display()))?;
    fs::rename(&temp, &target).map_err(|e| format!("{}: {e}", target.display()))?;

    for stale in existing_previews_for(path_str, PreviewKind::Preview) {
        if stale != target {
            let _ = fs::remove_file(stale);
        }
    }

    Ok(target)
}

/// The width a thumbnail regeneration should render at, so its own output can
/// serve as this photo's preview as well.
///
/// `None` for a photo with no preview, which is most of them: this keeps the
/// cache current for files the user asked to keep warm without quietly building
/// previews for every file they happen to edit. A thumbnail is a downscale of
/// whatever it is handed, so rendering wider costs only the render.
pub fn refresh_width_for(path_str: &str, app_handle: &AppHandle) -> Option<u32> {
    let (source, _) = parse_virtual_path(path_str);
    let dir = cache_dir_for(source.parent()?, PreviewKind::Preview);
    // One stat, and the answer for every folder that has never had a preview
    // built, which is most of them. Both steps below cost more than that: the
    // width comes from settings, which are read from disk, and the last one
    // lists a directory that can hold a frame per photo in the shoot. Neither
    // is worth doing on the way to generating an ordinary thumbnail.
    if !dir.is_dir() {
        return None;
    }

    let width = preview_width(app_handle);
    let already_current = preview_path_for(path_str, width, PreviewKind::Preview)
        .map(|p| p.exists())
        .unwrap_or(false);
    if already_current {
        return None;
    }

    // Only now, and only for a photo whose current key is missing: is there an
    // older one, meaning this is a photo somebody asked to keep warm?
    if existing_previews_for(path_str, PreviewKind::Preview).is_empty() {
        return None;
    }
    Some(width)
}

/// Renders and writes one preview.
///
/// Returns `Ok(None)` when the photo already has a current one, so a build over
/// a folder that is mostly done costs a directory lookup rather than a decode.
fn build_one_preview(
    path_str: &str,
    width: u32,
    app_handle: &AppHandle,
) -> Result<Option<PathBuf>, String> {
    let (source, _) = parse_virtual_path(path_str);
    if is_cloud_placeholder(&source) {
        return Err(format!("{} has not been downloaded yet", source.display()));
    }

    let target = preview_path_for(path_str, width, PreviewKind::Preview)
        .ok_or_else(|| format!("Cannot place a preview for {path_str}"))?;
    if target.exists() {
        return Ok(None);
    }

    let state = app_handle.state::<AppState>();
    let gpu_context = gpu_processing::get_or_init_gpu_context(&state, app_handle).ok();
    let rendered = generate_thumbnail_data(
        path_str,
        gpu_context.as_ref(),
        None,
        app_handle,
        Some(width),
    )
    .map_err(|e| format!("{path_str}: {e}"))?;

    store_rendered_preview(path_str, &rendered, width).map(Some)
}

#[derive(Clone, Serialize)]
struct PreviewProgress {
    current: usize,
    total: usize,
}

#[derive(Clone, Serialize)]
pub struct BuildPreviewsResult {
    pub built: usize,
    pub skipped: usize,
    pub failed: usize,
    /// The first thing that went wrong, so a folder that cannot be written to
    /// says so once rather than a thousand times.
    pub first_error: Option<String>,
}

/// Builds previews for a set of photos, bounded by RAM rather than by cores.
///
/// Each decode in flight is 545 MB on a 45 megapixel file, so the worker count
/// follows the thumbnail setting (four by default) instead of the processor
/// count. Eight workers would be 4.4 GB of peak resident memory for a job that
/// is already limited by the decode.
#[tauri::command]
pub async fn build_previews_for_paths(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<BuildPreviewsResult, String> {
    if paths.is_empty() {
        return Ok(BuildPreviewsResult {
            built: 0,
            skipped: 0,
            failed: 0,
            first_error: None,
        });
    }

    tauri::async_runtime::spawn_blocking(move || {
        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let width = preview_width(&app_handle);
        let workers = settings.thumbnail_worker_threads.unwrap_or(4).clamp(1, 8) as usize;

        let total = paths.len();
        let done = AtomicUsize::new(0);
        let built = AtomicUsize::new(0);
        let skipped = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);
        let first_error = std::sync::Mutex::new(None::<String>);

        let _ = app_handle.emit(
            "preview-build-progress",
            PreviewProgress { current: 0, total },
        );

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .map_err(|e| e.to_string())?;

        pool.install(|| {
            paths.par_iter().for_each(|path| {
                match build_one_preview(path, width, &app_handle) {
                    Ok(Some(_)) => {
                        built.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(None) => {
                        skipped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        let mut slot = first_error.lock().unwrap();
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                    }
                }
                let current = done.fetch_add(1, Ordering::Relaxed) + 1;
                let _ =
                    app_handle.emit("preview-build-progress", PreviewProgress { current, total });
            });
        });

        let _ = app_handle.emit(
            "preview-build-progress",
            PreviewProgress {
                current: 0,
                total: 0,
            },
        );

        Ok(BuildPreviewsResult {
            built: built.load(Ordering::Relaxed),
            skipped: skipped.load(Ordering::Relaxed),
            failed: failed.load(Ordering::Relaxed),
            first_error: first_error.lock().unwrap().clone(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Throws away every cached derivative for these photos, current or stale, and
/// removes the folder when it empties so a shoot with no previews carries no
/// trace of the cache.
#[tauri::command]
pub async fn discard_previews_for_paths(paths: Vec<String>) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut removed = 0usize;
        let mut folders = std::collections::HashSet::new();

        for path in &paths {
            for file in existing_previews_for(path, PreviewKind::Preview) {
                if fs::remove_file(&file).is_ok() {
                    removed += 1;
                }
            }
            let (source, _) = parse_virtual_path(path);
            if let Some(folder) = source.parent() {
                folders.insert(folder.to_path_buf());
            }
        }

        for folder in folders {
            let kind_dir = cache_dir_for(&folder, PreviewKind::Preview);
            let _ = fs::remove_dir(&kind_dir);
            let _ = fs::remove_dir(folder.join(CACHE_DIR_NAME));
        }

        Ok(removed)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The cached preview for one photo, as a data URL, or nothing.
///
/// Bytes rather than a path because the asset protocol is scoped to the app
/// cache folder, and widening that to every drive a photo might sit on to save
/// an encode of 172 KB would be a poor trade.
#[tauri::command]
pub async fn cached_preview_for_path(
    path: String,
    app_handle: AppHandle,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let width = preview_width(&app_handle);
        let target = preview_path_for(&path, width, PreviewKind::Preview)?;
        let bytes = fs::read(&target).ok()?;
        Some(format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    })
    .await
    .map_err(|e| e.to_string())
}

/// The small picture to nudge, and nothing that is not provably of this photo.
///
/// The preview first, because it is the bigger of the two and is the picture
/// the canvas is already showing while the decode runs, so a nudge on it is a
/// change to what is on screen rather than a swap to something else. Its file
/// name carries a hash of the adjustments it was rendered with, so a file that
/// is there at all is by construction the current one.
///
/// Then the thumbnail, if it can be proved current the same way.
///
/// Then nothing. Not the raw's own embedded JPEG: that is the camera's
/// rendering rather than this program's, so nudging it would show a picture
/// that jumps in look the moment the real render lands, which is worse than a
/// preview that does not move.
pub fn small_picture_to_nudge(path_str: &str, app_handle: &AppHandle) -> Option<PathBuf> {
    let width = preview_width(app_handle);
    if let Some(preview) = preview_path_for(path_str, width, PreviewKind::Preview)
        && preview.exists()
    {
        return Some(preview);
    }
    crate::file_management::current_thumbnail_for(path_str, app_handle)
}

/// How many of these photos already have a current preview, for a menu that
/// should say Build or Rebuild rather than guessing.
#[tauri::command]
pub async fn count_cached_previews(
    paths: Vec<String>,
    app_handle: AppHandle,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let width = preview_width(&app_handle);
        paths
            .iter()
            .filter(|path| {
                preview_path_for(path, width, PreviewKind::Preview)
                    .map(|p| p.exists())
                    .unwrap_or(false)
            })
            .count()
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blitzraw-preview-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_cache_sits_under_the_folder_it_describes() {
        let folder = Path::new("D:/MyPhotos/shoot");
        assert_eq!(
            cache_dir_for(folder, PreviewKind::Preview),
            Path::new("D:/MyPhotos/shoot/.blitzraw-previews/preview")
        );
    }

    #[test]
    fn a_virtual_copy_does_not_share_a_file_name_with_its_original() {
        let original = file_stem_for("D:/photos/_DSC1.dng");
        let copy = file_stem_for("D:/photos/_DSC1.dng?vc=2");
        assert_ne!(original, copy);
        assert!(
            !copy.contains('?'),
            "a file name cannot carry the vc marker: {copy}"
        );
        assert!(
            !copy.contains('='),
            "a file name cannot carry the vc marker: {copy}"
        );
    }

    #[test]
    fn the_key_moves_when_anything_it_depends_on_moves() {
        let dir = temp_dir("key");
        let file = dir.join("a.dng");
        fs::write(&file, b"pixels").unwrap();
        let path = file.to_string_lossy().to_string();

        let base = cache_key(&path, b"{}", 1920, PreviewKind::Preview).unwrap();
        assert_eq!(
            base,
            cache_key(&path, b"{}", 1920, PreviewKind::Preview).unwrap()
        );

        assert_ne!(
            base,
            cache_key(&path, b"{\"exposure\":1}", 1920, PreviewKind::Preview).unwrap(),
            "an edit must not reuse the old preview"
        );
        assert_ne!(
            base,
            cache_key(&path, b"{}", 2560, PreviewKind::Preview).unwrap(),
            "raising the preview resolution must not show the old smaller one"
        );

        // Re-merging an HDR writes the same path with different pixels. This is
        // the case that once left a ghosted merge on screen until a restart.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fs::write(&file, b"different pixels, and more of them").unwrap();
        assert_ne!(
            base,
            cache_key(&path, b"{}", 1920, PreviewKind::Preview).unwrap(),
            "a rewritten file must not reuse the old preview"
        );
    }

    #[test]
    fn previews_are_found_by_stem_whatever_key_they_carry() {
        let dir = temp_dir("stems");
        let file = dir.join("a.dng");
        fs::write(&file, b"pixels").unwrap();
        let path = file.to_string_lossy().to_string();

        let cache = cache_dir_for(&dir, PreviewKind::Preview);
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("a.dng__0123456789abcdef.jpg"), b"old").unwrap();
        fs::write(cache.join("a.dng__fedcba9876543210.jpg"), b"older").unwrap();
        fs::write(cache.join("b.dng__0123456789abcdef.jpg"), b"someone else").unwrap();

        let found = existing_previews_for(&path, PreviewKind::Preview);
        assert_eq!(
            found.len(),
            2,
            "both keys for a.dng, and nothing belonging to b.dng"
        );
    }

    #[test]
    fn storing_a_render_replaces_the_key_it_was_under() {
        let dir = temp_dir("store");
        let file = dir.join("a.dng");
        fs::write(&file, b"pixels").unwrap();
        let path = file.to_string_lossy().to_string();

        // A preview left over from before an edit, under some other key.
        let cache = cache_dir_for(&dir, PreviewKind::Preview);
        fs::create_dir_all(&cache).unwrap();
        let stale = cache.join("a.dng__0123456789abcdef.jpg");
        fs::write(&stale, b"old").unwrap();

        let rendered =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(400, 300, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
            }));
        let written = store_rendered_preview(&path, &rendered, 200).unwrap();

        assert!(written.exists(), "the new preview is on disk");
        assert!(!stale.exists(), "and the key it replaced is gone");
        assert_eq!(
            existing_previews_for(&path, PreviewKind::Preview).len(),
            1,
            "exactly one preview per photo"
        );

        let decoded = image::open(&written).unwrap();
        assert_eq!(decoded.width(), 200, "stored at the width it was asked for");
        assert!(
            !written.with_extension("jpg.part").exists(),
            "no part file left behind"
        );
    }

    #[test]
    fn a_render_narrower_than_the_target_is_not_stretched() {
        let dir = temp_dir("narrow");
        let file = dir.join("a.dng");
        fs::write(&file, b"pixels").unwrap();
        let path = file.to_string_lossy().to_string();

        let rendered = image::DynamicImage::ImageRgb8(image::RgbImage::new(120, 90));
        let written = store_rendered_preview(&path, &rendered, 1920).unwrap();
        let decoded = image::open(&written).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (120, 90));
    }

    #[test]
    fn a_photo_with_no_cache_folder_has_no_previews() {
        let dir = temp_dir("empty");
        let file = dir.join("a.dng");
        fs::write(&file, b"pixels").unwrap();
        assert!(existing_previews_for(&file.to_string_lossy(), PreviewKind::Preview).is_empty());
    }
}

// ======== BLITZRAW: what a preview key is allowed to depend on ========
#[cfg(test)]
mod blitzraw_portable_preview_tests {
    use super::*;

    /// The same photo in two places gets the same key.
    ///
    /// Goes through `cache_key` and real files rather than through
    /// `identity_in_folder`, because the key is the thing that changed and a
    /// test of the helper alone passes whether or not the key uses it. It was
    /// written the other way first and passed with the fault put back, which is
    /// the only reason this is worth saying.
    #[test]
    fn a_photo_that_moved_is_still_the_same_photo() {
        let root = std::env::temp_dir().join("blitzraw-portable-preview");
        let working = root.join("working");
        let archive = root.join("archive");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&working).expect("a working folder");
        fs::create_dir_all(&archive).expect("an archive folder");

        let here = working.join("_DSC3688_Hdr.dng");
        let there = archive.join("_DSC3688_Hdr.dng");
        fs::write(&here, b"not really a photo").expect("write");
        // As copying a shoot to the archive does, timestamp and all.
        fs::copy(&here, &there).expect("copy");

        let one = cache_key(&here.to_string_lossy(), b"{}", 1920, PreviewKind::Preview);
        let two = cache_key(&there.to_string_lossy(), b"{}", 1920, PreviewKind::Preview);
        assert!(one.is_some() && two.is_some(), "both files are there");
        assert_eq!(
            one, two,
            "a shoot moving to the archive must not throw away its previews"
        );

        // And the things a preview really does depend on still move it.
        assert_ne!(
            one,
            cache_key(
                &here.to_string_lossy(),
                b"{\"exposure\":1}",
                1920,
                PreviewKind::Preview
            ),
            "an edit has to make a new preview"
        );
        assert_ne!(
            one,
            cache_key(&here.to_string_lossy(), b"{}", 720, PreviewKind::Preview),
            "and so does a different width"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// And two photos in one folder are still two photos.
    #[test]
    fn two_photos_in_one_folder_stay_apart() {
        let one = r"D:\shoot\_DSC0001.NEF";
        let two = r"D:\shoot\_DSC0002.NEF";
        assert_ne!(identity_in_folder(one), identity_in_folder(two));
    }

    /// A virtual copy is its own picture, and cannot be confused with a photo
    /// whose name happens to end in the copy's number.
    #[test]
    fn a_virtual_copy_is_its_own_picture() {
        let original = r"D:\shoot.dng";
        let copy = r"D:\shoot.dng?vc=1";
        let other_copy = r"D:\shoot.dng?vc=2";
        assert_ne!(identity_in_folder(original), identity_in_folder(copy));
        assert_ne!(identity_in_folder(copy), identity_in_folder(other_copy));
        assert_ne!(
            identity_in_folder(r"D:\shoot?vc=1"),
            identity_in_folder(r"D:\shoot1")
        );
    }

    /// Names the on-disk stem would flatten together are still two files.
    #[test]
    fn names_that_only_differ_by_a_space_are_two_files() {
        assert_ne!(
            identity_in_folder(r"D:\shoot b.dng"),
            identity_in_folder(r"D:\shoot_b.dng")
        );
        assert_eq!(
            file_stem_for(r"D:\shoot b.dng"),
            file_stem_for(r"D:\shoot_b.dng"),
            "which is exactly why the key cannot use the stem"
        );
    }
}
// ====== BLITZRAW END: what a preview key is allowed to depend on ======

// ======== BLITZRAW: the picture the scopes read, on a real shoot ========
#[cfg(test)]
mod blitzraw_small_picture_probe {
    use super::*;

    /// That a real photo's preview is found, and that scopes come off it.
    ///
    /// The lookup is the risky half of showing scopes before a decode: the
    /// maths is the same maths the analytics worker already runs, and the only
    /// new thing is finding a picture to run it on. So this points at a real
    /// shoot and checks there is one.
    ///
    /// ```text
    /// RAPIDRAW_TEST_PREVIEWED_PHOTO="D:/MyPhotos/.../_DSC3688_Hdr.dng" cargo test --lib -- --nocapture report_the_small_picture
    /// ```
    #[test]
    fn report_the_small_picture() {
        let Ok(path) = std::env::var("RAPIDRAW_TEST_PREVIEWED_PHOTO") else {
            eprintln!("RAPIDRAW_TEST_PREVIEWED_PHOTO unset, skipping");
            return;
        };
        let found = newest_preview_for(&path);
        eprintln!("preview for {path}: {found:?}");
        let Some(found) = found else {
            panic!("no preview found, so the scopes would have nothing to read");
        };

        let began = std::time::Instant::now();
        let bytes = std::fs::read(&found).expect("read the preview");
        let image = image::load_from_memory(&bytes).expect("decode the preview");
        let decoded = began.elapsed();

        let began = std::time::Instant::now();
        let histogram = crate::image_processing::calculate_histogram_from_image(&image)
            .expect("a histogram off the preview");
        eprintln!(
            "{}x{} decoded in {decoded:?}, histogram in {:?}",
            image.width(),
            image.height(),
            began.elapsed()
        );
        let as_json = serde_json::to_value(&histogram).expect("serialise");
        let luma = as_json["luma"].as_array().expect("a luma channel");
        assert_eq!(luma.len(), 256, "the shape the front end draws");
        assert!(
            luma.iter().any(|v| v.as_f64().unwrap_or(0.0) > 0.0),
            "a real photo has something in its histogram"
        );
    }
}
// ====== BLITZRAW END: the picture the scopes read, on a real shoot ======
