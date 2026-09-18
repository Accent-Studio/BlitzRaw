//! Moving the picture on screen before the raw has finished decoding.
//!
//! # The gap this fills
//!
//! Arrow to the next frame and press exposure down, and for one to three
//! seconds nothing happens on the large preview. The press is not lost any more
//! (see `pendingNudges.ts`), but the picture does not move until the decode
//! lands, and a second and a half of no feedback while working down a shoot is
//! a long time.
//!
//! The grid does not have this problem, and it is worth being exact about why,
//! because the reason is not what it looks like. A grid thumbnail is not a
//! trick: it is a real render. It is quick because exposure is deliberately not
//! part of `calculate_thumbnail_base_hash`, so the expensive half of the work,
//! the decode and the geometry, is still sitting in `thumbnail_geometry_cache`
//! and only the GPU pass runs. Eleven to forty-five milliseconds, against six
//! hundred for the decode.
//!
//! The large preview cannot do that on a photo that has never been decoded,
//! because there is no base in memory to reuse. But there is one on disk: the
//! rendered preview in `.blitzraw-previews`, which is this photo with its own
//! adjustments already in it, at 1920 pixels.
//!
//! # Why the nudge goes on as a difference
//!
//! The picture on disk is not the photo, it is the photo **as currently
//! adjusted**. Applying the new exposure to it would apply it twice. What goes
//! on is the difference: what the adjustments are now, minus what they were
//! when the photo was opened, which is exactly the nudge and nothing else.
//!
//! The front end supplies both halves rather than this reading the sidecar,
//! because the sidecar is written on a delay and a held key would have it
//! part-way through the run.
//!
//! # What is exact and what is not
//!
//! **White balance is exact.** For a file with a camera calibration, Kelvin is
//! not a multiplier here but a matrix, and one white balance composed with the
//! inverse of another is precisely the difference between them. See
//! `CameraProfile::correction_between`.
//!
//! **Exposure is exact as a number and approximate as pixels.** The shader
//! applies it in linear light before the tone curve and the tonemapper. On the
//! proxy those have already run, so the same gain lands after the curve rather
//! than before it. At a tenth of a stop the difference is invisible in the
//! midtones and shows only where the curve is steep.
//!
//! **Highlights do not come back.** The source is an eight-bit JPEG already
//! clipped at white, so pulling exposure down on a blown sky gives grey where
//! the real render finds cloud. That is the sharpest limit here and it is worth
//! knowing rather than working around: the real render replaces this within a
//! couple of seconds and shows the truth.

use crate::AppState;
use image::DynamicImage;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use crate::image_processing::{AllAdjustments, get_all_adjustments_from_json, rows_to_gpu_mat3};
use serde_json::Value;
use tauri::AppHandle;
use tauri::Manager;

// ============ BLITZRAW: the source picture is decoded once ============
/// The last small picture that was nudged, kept decoded.
///
/// A press costs about seventy milliseconds on the GPU. Decoding the same 1920
/// pixel JPEG again for each one costs about as much again, which is the
/// difference between a picture that moves while the key is held and one that
/// catches up afterwards.
///
/// One entry. Nudging is something you do to the photo in front of you, and
/// holding more would be megabytes kept against a second photo nobody is
/// nudging. Replaced rather than grown when the path changes.
static DECODED_SOURCE: LazyLock<Mutex<Option<(PathBuf, Arc<DynamicImage>)>>> =
    LazyLock::new(|| Mutex::new(None));

fn source_picture(source: &Path) -> Option<Arc<DynamicImage>> {
    if let Ok(held) = DECODED_SOURCE.lock()
        && let Some((path, picture)) = held.as_ref()
        && path == source
    {
        return Some(picture.clone());
    }

    let picture = Arc::new(image::open(source).ok()?);
    if let Ok(mut held) = DECODED_SOURCE.lock() {
        *held = Some((source.to_path_buf(), picture.clone()));
    }
    Some(picture)
}

/// Lets go of the decoded picture. Called when a photo's real render arrives,
/// since nothing will be nudging its stand-in again.
pub fn forget_source() {
    if let Ok(mut held) = DECODED_SOURCE.lock() {
        *held = None;
    }
}
// ========== BLITZRAW END: the source picture is decoded once ==========

/// A white balance as the editor holds it.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct WhitePoint {
    pub kelvin: f32,
    pub tint: f32,
}

/// Everything that moved since the photo was opened.
///
/// Only the adjustments a keyboard nudge can reach. Anything else is left for
/// the real render, because a difference is only meaningful for a value that
/// composes, and most adjustments do not.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Nudge {
    /// Stops, signed. Zero when exposure did not move.
    #[serde(default)]
    pub exposure: f32,
    /// Where the white balance was when the picture on disk was rendered.
    #[serde(default)]
    pub white_balance_from: Option<WhitePoint>,
    /// And where it is now.
    #[serde(default)]
    pub white_balance_to: Option<WhitePoint>,
}

impl Nudge {
    /// Whether there is anything here worth rendering for.
    ///
    /// A press that ran into a limit moves nothing, and rendering the picture
    /// unchanged would replace what is on screen with an identical copy of
    /// itself, which is work for a flicker.
    pub fn moves_anything(&self) -> bool {
        if self.exposure.abs() > 1e-6 {
            return true;
        }
        match (self.white_balance_from, self.white_balance_to) {
            (Some(from), Some(to)) => {
                (from.kelvin - to.kelvin).abs() > 1e-3 || (from.tint - to.tint).abs() > 1e-3
            }
            _ => false,
        }
    }
}

/// The adjustments to render the small picture with: neutral, plus the nudge.
///
/// Neutral matters. The picture already has the photo's whole develop in it, so
/// anything set here that is not the difference would be applied a second time.
fn adjustments_for(nudge: &Nudge, path: &str) -> AllAdjustments {
    // `false` for is_raw and no tonemapper override: the source is a developed
    // sRGB JPEG, so it must not be tone-mapped again.
    let mut all = get_all_adjustments_from_json(&serde_json::json!({}), false, None);
    all.global.exposure = nudge.exposure;

    let (Some(from), Some(to)) = (nudge.white_balance_from, nudge.white_balance_to) else {
        return all;
    };
    if (from.kelvin - to.kelvin).abs() < 1e-3 && (from.tint - to.tint).abs() < 1e-3 {
        return all;
    }

    match crate::camera_profile::profile_for(path)
        .and_then(|profile| profile.correction_between(from.kelvin, from.tint, to.kelvin, to.tint))
    {
        Some(correction) => {
            all.global.camera_to_working = rows_to_gpu_mat3(correction);
            all.global.use_camera_profile = 1;
        }
        None => {
            // No calibration to work from, so no Kelvin either: on such a file
            // the editor's temperature is the old relative one, and the nudge
            // arrives as a plain difference the shader applies directly.
            log::debug!("No usable white balance matrix for {path}; the nudge keeps its exposure only");
        }
    }
    all
}

/// The small picture with the nudge on it, as a data URL, or nothing.
///
/// Nothing is the honest answer for a photo with no preview and no current
/// thumbnail: there is no picture of it that this program made, and the
/// camera's own is a different rendering that would jump when the real one
/// arrives.
#[tauri::command]
pub async fn render_nudged_preview(
    path: String,
    nudge: Nudge,
    app_handle: AppHandle,
) -> Result<Option<String>, String> {
    if !nudge.moves_anything() {
        return Ok(None);
    }

    tauri::async_runtime::spawn_blocking(move || {
        let Some(source) = crate::preview_cache::small_picture_to_nudge(&path, &app_handle) else {
            return Ok(None);
        };
        let Some(picture) = source_picture(&source) else {
            log::warn!("Could not open {} to nudge it", source.display());
            return Ok(None);
        };

        let state = app_handle.state::<AppState>();
        let context = crate::gpu_processing::get_or_init_gpu_context(&state, &app_handle)?;

        // Its own hash space, so a proxy render can never be served from, or
        // land in, the cache the real renders share.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&"proxy", &mut hasher);
        std::hash::Hash::hash(&path, &mut hasher);
        std::hash::Hash::hash(&nudge.exposure.to_bits(), &mut hasher);
        if let (Some(from), Some(to)) = (nudge.white_balance_from, nudge.white_balance_to) {
            for value in [from.kelvin, from.tint, to.kelvin, to.tint] {
                std::hash::Hash::hash(&value.to_bits(), &mut hasher);
            }
        }
        let unique_hash = std::hash::Hasher::finish(&hasher);

        let rendered = crate::gpu_processing::process_and_get_dynamic_image(
            &context,
            &state,
            picture.as_ref(),
            unique_hash,
            crate::gpu_processing::RenderRequest {
                adjustments: adjustments_for(&nudge, &path),
                mask_bitmaps: &[],
                lut: None,
                roi: None,
            },
            "render_nudged_preview",
        )?;

        let bytes = crate::file_management::encode_thumbnail(&rendered, rendered.width())
            .map_err(|e| e.to_string())?;
        Ok(Some(format!(
            "data:image/jpeg;base64,{}",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
        )))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Told when a photo's real render has arrived, so the stand-in it was using
/// can be let go rather than held until the next photo needs the room.
#[tauri::command]
pub fn forget_nudged_source() {
    forget_source();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decoded picture is kept for one photo, and swapped rather than
    /// grown when the pointer moves to another.
    #[test]
    fn the_source_picture_is_kept_for_one_photo_at_a_time() {
        let dir = std::env::temp_dir().join("blitzraw-proxy-source");
        let _ = std::fs::create_dir_all(&dir);
        let one = dir.join("one.jpg");
        let two = dir.join("two.jpg");
        image::DynamicImage::new_rgb8(8, 8).save(&one).unwrap();
        image::DynamicImage::new_rgb8(16, 16).save(&two).unwrap();

        forget_source();
        let first = source_picture(&one).expect("opened");
        let again = source_picture(&one).expect("opened");
        assert!(Arc::ptr_eq(&first, &again), "the same photo is not decoded twice");

        let other = source_picture(&two).expect("opened");
        assert_eq!(other.width(), 16, "and another photo is decoded");
        let back = source_picture(&one).expect("opened");
        assert!(!Arc::ptr_eq(&first, &back), "only one is held at a time");

        forget_source();
    }

    fn white(kelvin: f32, tint: f32) -> Option<WhitePoint> {
        Some(WhitePoint { kelvin, tint })
    }

    /// A press that ran into a limit moved nothing, and rendering an identical
    /// copy of what is already on screen is work for a flicker.
    #[test]
    fn a_nudge_that_moved_nothing_is_not_worth_a_render() {
        assert!(!Nudge::default().moves_anything());
        assert!(
            !Nudge {
                exposure: 0.0,
                white_balance_from: white(5000.0, 10.0),
                white_balance_to: white(5000.0, 10.0),
            }
            .moves_anything()
        );
    }

    #[test]
    fn a_tenth_of_a_stop_is_worth_a_render() {
        assert!(
            Nudge {
                exposure: 0.1,
                ..Default::default()
            }
            .moves_anything()
        );
    }

    #[test]
    fn fifty_kelvin_is_worth_a_render() {
        assert!(
            Nudge {
                exposure: 0.0,
                white_balance_from: white(5000.0, 10.0),
                white_balance_to: white(5050.0, 10.0),
            }
            .moves_anything()
        );
    }

    /// A white balance with only one end known is not a difference, and
    /// guessing the other end would re-white-balance the photo.
    #[test]
    fn half_a_white_balance_is_not_a_difference() {
        assert!(
            !Nudge {
                exposure: 0.0,
                white_balance_from: None,
                white_balance_to: white(5050.0, 10.0),
            }
            .moves_anything()
        );
    }

    /// Neutral except for the nudge. Anything else set here would be applied on
    /// top of a picture that already has the photo's whole develop in it.
    #[test]
    fn the_render_is_neutral_apart_from_the_nudge() {
        let all = adjustments_for(
            &Nudge {
                exposure: -0.3,
                ..Default::default()
            },
            "D:\\nowhere\\photo.nef",
        );
        assert!((all.global.exposure - -0.3).abs() < 1e-6);
        assert_eq!(all.global.contrast, 0.0);
        assert_eq!(all.global.saturation, 0.0);
        assert_eq!(all.global.highlights, 0.0);
        assert_eq!(all.global.shadows, 0.0);
        assert_eq!(
            all.global.use_camera_profile, 0,
            "a file with no calibration gets no matrix"
        );
    }
}
