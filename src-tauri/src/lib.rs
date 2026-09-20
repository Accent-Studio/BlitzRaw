// Clippy lints this crate does not take, and the reason for each. CI runs
// clippy with `-D warnings`, so a lint that is not wanted has to be named here
// rather than left as noise that hides the next real one.
//
// `needless_range_loop`: the colour and merge maths index several arrays from
//   one counter. Written as iterators they become zips and enumerates that no
//   longer read like the formula they implement, in code whose output has been
//   measured against real files. Clarity here is worth more than the lint.
//
// `neg_cmp_op_on_partial_ord`: `!(x >= 0.0)` is not `x < 0.0`. The first is
//   true for NaN and the second is false for it, and these comparisons are
//   guards that must reject NaN. Taking this lint would quietly let NaN
//   through, which is the bug the guard exists to stop.
//
// `too_many_arguments` and `type_complexity`: the decode and merge paths really
//   do take that many settings. Bundling them into a struct to satisfy a
//   counter only hides which caller passes what.
//
// `field_reassign_with_default`: building a default and then setting fields
//   reads in the order the work happens, which is how these were written.
#![allow(
    clippy::needless_range_loop,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::field_reassign_with_default
)]

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
use mimalloc::MiMalloc;

#[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod adjustment_utils;
mod ai_commands;
mod ai_connector;
mod ai_processing;
mod android_integration;
mod app_settings;
mod app_state;
mod auto_stack;
mod bulk_hdr;
mod cache_utils;
mod camera_profile;
mod culling;
mod data_dir;
mod denoising;
mod dng_convert;
// BLITZRAW: a photo's edit history, kept in its sidecar.
mod edit_history;
mod embedded_xmp;
mod exif_processing;
mod export_processing;
mod file_management;
mod formats;
mod gpu_processing;
mod hdr_deghosting;
mod hdr_dng;
mod hdr_merge;
mod image_loader;
mod image_processing;
mod inpainting;
mod launch_request;
mod legacy_names;
mod lens_blur;
mod lens_correction;
mod library_ignore;
mod log_sink;
mod lut_processing;
mod mask_generation;
mod multi_exposure;
mod nef_compression;
mod negative_conversion;
mod panel_window;
mod panorama_stitching;
mod panorama_utils;
// BLITZRAW: the vector pen mask, drawn as a path rather than painted.
mod pen_mask;
mod preset_converter;
mod preview_cache;
// BLITZRAW: the picture moves before the raw has finished decoding.
mod proxy_preview;
mod raw_processing;
mod resilient_emit;
// BLITZRAW: the one door through which a photo's sidecar is written.
mod sidecar;
mod stacks;
mod tagging;
mod tagging_utils;
mod window_customizer;
mod window_places;

use std::collections::{HashMap, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::io::Write;
use std::panic;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Luma, RgbImage, Rgba};
use imageproc::drawing::draw_line_segment_mut;
use imageproc::edges::canny;
use imageproc::hough::{LineDetectionOptions, detect_lines};
use imgref::ImgRef;
use mozjpeg_rs::{Encoder, Preset};
use rgb::{FromSlice, RGBA8};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{Emitter, Manager, ipc::Response};
use tempfile::NamedTempFile;
use tokio::sync::Mutex as TokioMutex;

#[cfg(target_os = "linux")]
use webkit2gtk_nvidia_quirk::{
    ApplyWorkaroundOptions, WorkaroundKind, apply_workaround_with_options, needs_workaround,
};

use crate::cache_utils::{
    DecodedImageCache, GEOMETRY_KEYS, calculate_full_job_hash, calculate_geometry_hash,
    calculate_transform_hash, calculate_visual_hash,
};
use crate::file_management::{parse_virtual_path, read_file_mapped};
use crate::formats::is_raw_file;
use crate::hdr_deghosting::{align_hdr_frames, assert_uniform_dimensions, load_hdr_frames};
use crate::image_loader::{composite_patches_on_image, load_and_composite};
use crate::image_processing::{
    Crop, GeometryParams, RenderRequest, apply_coarse_rotation, apply_cpu_default_raw_processing,
    apply_flip, apply_geometry_warp, apply_linear_to_srgb, downscale_f32_image,
    get_all_adjustments_from_json, get_or_init_gpu_context, process_and_get_dynamic_image,
    resolve_tonemapper_override, resolve_tonemapper_override_from_handle, warp_image_geometry,
};
use crate::mask_generation::{
    MaskDefinition, generate_mask_bitmap, get_cached_or_generate_mask,
    resolve_warped_image_for_masks,
};
use crate::window_customizer::PinchZoomDisablePlugin;
pub use adjustment_utils::*;
pub use android_integration::*;
pub use app_settings::*;
pub use app_state::*;
pub use launch_request::*;
use tagging_utils::{candidates, hierarchy};

#[cfg(target_os = "macos")]
extern "C" fn force_exit(_signal: libc::c_int) {
    unsafe {
        libc::_exit(0);
    }
}

#[cfg(target_os = "macos")]
pub fn register_exit_handler() {
    unsafe {
        libc::signal(libc::SIGABRT, force_exit as *const () as libc::sighandler_t);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn register_exit_handler() {}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommunityPreset {
    pub name: String,
    pub creator: String,
    pub adjustments: Value,
    #[serde(rename = "includeMasks")]
    pub include_masks: Option<bool>,
    #[serde(rename = "includeCropTransform")]
    pub include_crop_transform: Option<bool>,
}

#[derive(serde::Serialize)]
struct ImageDimensions {
    width: u32,
    height: u32,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WgpuTransformPayload {
    pub window_width: f32,
    pub window_height: f32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub clip_x: f32,
    pub clip_y: f32,
    pub clip_width: f32,
    pub clip_height: f32,
    pub bg_primary: [f32; 4],
    pub bg_secondary: [f32; 4],
    pub pixelated: bool,
}

pub fn generate_transformed_preview(
    state: &tauri::State<AppState>,
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
    preview_dim: u32,
) -> Result<(DynamicImage, f32, (f32, f32)), String> {
    let transform_hash = calculate_transform_hash(adjustments);

    let (transformed_full_res, unscaled_crop_offset) = {
        let mut cache_lock = state.full_transformed_cache.lock().unwrap();
        if let Some((hash, img, offset)) = cache_lock.as_ref() {
            if *hash == transform_hash {
                (Arc::clone(img), *offset)
            } else {
                let (arc_img, offset) = compute_full_transformed_res(loaded_image, adjustments)?;
                *cache_lock = Some((transform_hash, Arc::clone(&arc_img), offset));
                (arc_img, offset)
            }
        } else {
            let (arc_img, offset) = compute_full_transformed_res(loaded_image, adjustments)?;
            *cache_lock = Some((transform_hash, Arc::clone(&arc_img), offset));
            (arc_img, offset)
        }
    };

    let (full_res_w, full_res_h) = transformed_full_res.dimensions();

    let final_preview_base = if full_res_w > preview_dim || full_res_h > preview_dim {
        downscale_f32_image(&transformed_full_res, preview_dim, preview_dim)
    } else {
        (*transformed_full_res).clone()
    };

    let scale_for_gpu = if full_res_w > 0 {
        final_preview_base.width() as f32 / full_res_w as f32
    } else {
        1.0
    };

    Ok((final_preview_base, scale_for_gpu, unscaled_crop_offset))
}

fn compute_full_transformed_res(
    loaded_image: &LoadedImage,
    adjustments: &serde_json::Value,
) -> Result<(Arc<DynamicImage>, (f32, f32)), String> {
    let has_patches = adjustments
        .get("aiPatches")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    let patched_original_image = if has_patches {
        Cow::Owned(
            composite_patches_on_image(&loaded_image.image, adjustments)
                .map_err(|e| format!("Failed to composite AI patches: {}", e))?,
        )
    } else {
        Cow::Borrowed(loaded_image.image.as_ref())
    };

    let (transformed_img, offset) = apply_all_transformations(patched_original_image, adjustments);
    Ok((Arc::new(transformed_img.into_owned()), offset))
}

#[tauri::command]
fn get_image_dimensions(path: String) -> Result<ImageDimensions, String> {
    let (source_path, _) = parse_virtual_path(&path);
    image::image_dimensions(&source_path)
        .map(|(width, height)| ImageDimensions { width, height })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn cancel_thumbnail_generation(
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state
        .thumbnail_cancellation_token
        .store(true, Ordering::SeqCst);

    let mut tracker = state.thumbnail_progress.lock().unwrap();
    tracker.total = 0;
    tracker.completed = 0;
    drop(tracker);

    let _ = app_handle.emit(
        "thumbnail-progress",
        serde_json::json!({ "current": 0, "total": 0 }),
    );
    Ok(())
}

pub fn get_cached_full_warped_image(
    state: &tauri::State<AppState>,
    js_adjustments: &serde_json::Value,
) -> Result<Arc<DynamicImage>, String> {
    let geo_hash = calculate_geometry_hash(js_adjustments);

    {
        let cache_lock = state.full_warped_cache.lock().unwrap();
        if let Some((hash, img)) = cache_lock.as_ref()
            && *hash == geo_hash
        {
            return Ok(Arc::clone(img));
        }
    }

    let (base_arc, is_raw) = get_original_image(state)?;
    let mut cow_image = Cow::Borrowed(base_arc.as_ref());

    if is_raw {
        apply_cpu_default_raw_processing(cow_image.to_mut());
    }

    let warped_image = apply_geometry_warp(cow_image, js_adjustments).into_owned();
    let warped_arc = Arc::new(warped_image);

    {
        let mut cache_lock = state.full_warped_cache.lock().unwrap();
        *cache_lock = Some((geo_hash, Arc::clone(&warped_arc)));
    }

    Ok(warped_arc)
}

#[tauri::command]
async fn update_wgpu_transform(
    payload: WgpuTransformPayload,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let context = match state.gpu_context.lock().unwrap().as_ref() {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    tokio::task::spawn_blocking(move || {
        let mut display_lock = context.display.lock().unwrap();
        if let Some(display) = display_lock.as_mut() {
            display.latest_transform.rect = [payload.x, payload.y, payload.width, payload.height];
            display.latest_transform.clip = [
                payload.clip_x,
                payload.clip_y,
                payload.clip_width,
                payload.clip_height,
            ];
            display.latest_transform.window = [payload.window_width, payload.window_height];
            display.latest_transform.bg_primary = payload.bg_primary;
            display.latest_transform.bg_secondary = payload.bg_secondary;
            display.latest_transform.pixelated = if payload.pixelated { 1.0 } else { 0.0 };

            context.queue.write_buffer(
                &display.transform_buffer,
                0,
                bytemuck::bytes_of(&display.latest_transform),
            );
            display.render(&context.device, &context.queue);
        }
    })
    .await
    .map_err(|e| format!("Task panicked: {}", e))?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
// ============ BLITZRAW: waiting for a decode is not a fault ============
/// What a render says when there is no photo in hand to render.
///
/// `load_image` clears the held image before it starts decoding, and a Z9 frame
/// takes about a second and a half, so anything asking for a render in that gap
/// finds nothing. That is the ordinary way opening a photo goes: the metadata
/// arrives first, the editor sets the adjustments it just read, and that asks
/// for a render before the pixels exist.
///
/// Nothing is lost by it. When the decode lands, `load_image` returns and the
/// editor sets the size it learned, which asks for the render again.
///
/// It was logged as an error, nineteen times in one session. This project reads
/// its log to work out what went wrong, so a line saying ERROR about the normal
/// way a photo opens costs more than it sounds: it is what you find when you go
/// looking for the cause of something else.
pub const NOTHING_LOADED_YET: &str = "No original image loaded";
// ========== BLITZRAW END: waiting for a decode is not a fault ==========

fn process_preview_job(
    app_handle: &tauri::AppHandle,
    state: tauri::State<AppState>,
    mut adjustments_json: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    roi: Option<(f32, f32, f32, f32)>,
    compute_waveform: bool,
    active_waveform_channel: Option<&str>,
) -> Result<Vec<u8>, String> {
    let fn_start = std::time::Instant::now();
    let context = get_or_init_gpu_context(&state, app_handle)?;
    hydrate_adjustments(&state, &mut adjustments_json);
    let adjustments_clone = adjustments_json;

    let loaded_image_guard = state.original_image.lock().unwrap();
    let loaded_image = loaded_image_guard
        .as_ref()
        .ok_or(NOTHING_LOADED_YET)?
        .clone();
    drop(loaded_image_guard);

    let new_transform_hash = calculate_transform_hash(&adjustments_clone);
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let live_quality = settings.live_preview_quality.as_deref().unwrap_or("high");

    let default_preview_dim = settings.editor_preview_resolution.unwrap_or(1920);
    let preview_dim = target_resolution.unwrap_or(default_preview_dim);
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let use_wgpu_renderer = settings.use_wgpu_renderer.unwrap_or(true);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let use_wgpu_renderer = false;

    let has_roi = roi.is_some();
    let (interactive_divisor, interactive_quality) = match live_quality {
        "full" => (1.0_f32, 85_u8),
        "performance" => (if has_roi { 1.8_f32 } else { 1.5_f32 }, 65_u8),
        _ => (if has_roi { 1.4_f32 } else { 1.0_f32 }, 75_u8),
    };

    let mut cached_preview_lock = state.cached_preview.lock().unwrap();

    let base_valid = cached_preview_lock
        .as_ref()
        .is_some_and(|c| c.transform_hash == new_transform_hash && c.preview_dim == preview_dim);
    let small_valid = base_valid
        && cached_preview_lock
            .as_ref()
            .is_some_and(|c| c.interactive_divisor == interactive_divisor);

    let (final_preview_base, scale_for_gpu, unscaled_crop_offset) = if base_valid {
        let cached = cached_preview_lock.as_ref().unwrap();
        (
            Arc::clone(&cached.image),
            cached.scale,
            cached.unscaled_crop_offset,
        )
    } else {
        *state.gpu_image_cache.lock().unwrap() = None;

        let (base, scale, offset) =
            generate_transformed_preview(&state, &loaded_image, &adjustments_clone, preview_dim)?;
        (Arc::new(base), scale, offset)
    };

    let small_preview_base = if small_valid {
        Arc::clone(&cached_preview_lock.as_ref().unwrap().small_image)
    } else {
        let small = if interactive_divisor > 1.0 {
            let target_size = (preview_dim as f32 / interactive_divisor) as u32;
            let (w, h) = final_preview_base.dimensions();
            let (small_w, small_h) = if w > h {
                let ratio = h as f32 / w as f32;
                (target_size, (target_size as f32 * ratio) as u32)
            } else {
                let ratio = w as f32 / h as f32;
                ((target_size as f32 * ratio) as u32, target_size)
            };
            Arc::new(image_processing::downscale_f32_image(
                &final_preview_base,
                small_w,
                small_h,
            ))
        } else {
            Arc::clone(&final_preview_base)
        };

        if is_interactive && base_valid {
            *state.gpu_image_cache.lock().unwrap() = None;
        }

        small
    };

    *cached_preview_lock = Some(CachedPreview {
        image: Arc::clone(&final_preview_base),
        small_image: Arc::clone(&small_preview_base),
        transform_hash: new_transform_hash,
        scale: scale_for_gpu,
        unscaled_crop_offset,
        preview_dim,
        interactive_divisor,
    });

    drop(cached_preview_lock);

    let (processing_image, effective_scale, jpeg_quality) = if is_interactive {
        let orig_w = final_preview_base.width() as f32;
        let small_w = small_preview_base.width() as f32;
        let scale_factor = if orig_w > 0.0 { small_w / orig_w } else { 1.0 };
        let new_scale = scale_for_gpu * scale_factor;
        (small_preview_base, new_scale, interactive_quality)
    } else {
        (final_preview_base, scale_for_gpu, 94)
    };

    let (preview_width, preview_height) = processing_image.dimensions();

    let pixel_roi = if is_interactive {
        roi.map(|(nx, ny, nw, nh)| crate::gpu_processing::Roi {
            x: (nx * preview_width as f32).round() as u32,
            y: (ny * preview_height as f32).round() as u32,
            width: (nw * preview_width as f32).round() as u32,
            height: (nh * preview_height as f32).round() as u32,
        })
    } else {
        None
    };

    let mask_definitions: Vec<MaskDefinition> = adjustments_clone
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let scaled_crop_offset = (
        unscaled_crop_offset.0 * effective_scale,
        unscaled_crop_offset.1 * effective_scale,
    );

    let mask_bitmaps: Vec<ImageBuffer<Luma<u8>, Vec<u8>>> = mask_definitions
        .iter()
        .filter_map(|def| {
            get_cached_or_generate_mask(
                &state,
                def,
                preview_width,
                preview_height,
                effective_scale,
                scaled_crop_offset,
                &adjustments_clone,
            )
        })
        .collect();

    let is_raw = loaded_image.is_raw;
    let tm_override = resolve_tonemapper_override_from_handle(app_handle, is_raw);
    let mut final_adjustments =
        get_all_adjustments_from_json(&adjustments_clone, is_raw, tm_override);
    // BLITZRAW: white balance from the camera's own calibration.
    crate::image_processing::apply_camera_profile_to_adjustments(
        &mut final_adjustments,
        &loaded_image.path,
        &adjustments_clone,
    );
    let lut_path = adjustments_clone["lutPath"].as_str();
    let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());

    let wants_analytics = !(is_interactive && pixel_roi.is_some());
    // ============ BLITZRAW: ask for the scopes that are being looked at ============
    // This used to pass the requested list only while a slider was moving, and
    // None otherwise. None means "all of them" to
    // `calculate_waveform_from_image`, so every ordinary render computed five
    // scopes to show one or two, and the vectorscope's gain, which travels
    // inside the request as `vectorscope:3`, was thrown away with the rest of
    // it. The scope magnified while a slider moved and snapped back the instant
    // it was released, which is exactly the shape of a value that only survives
    // the interactive path.
    //
    // The front end sends what is on screen either way, so there is nothing to
    // choose between: passing it always is both correct and less work.
    let channel_filter = active_waveform_channel.map(|s| s.to_string());
    // ========== BLITZRAW END: ask for the scopes that are being looked at ==========

    let analytics_config = if wants_analytics {
        state
            .analytics_worker_tx
            .lock()
            .unwrap()
            .clone()
            .map(|tx| crate::AnalyticsConfig {
                path: loaded_image.path.clone(),
                compute_waveform,
                active_waveform_channel: channel_filter,
                sender: tx,
            })
    } else {
        None
    };

    let final_processed_image_result =
        crate::image_processing::process_and_get_dynamic_image_with_analytics(
            &context,
            &state,
            &processing_image,
            new_transform_hash,
            RenderRequest {
                adjustments: final_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: pixel_roi,
            },
            "apply_adjustments",
            use_wgpu_renderer,
            analytics_config,
        );

    if let Ok(final_processed_image) = final_processed_image_result {
        if use_wgpu_renderer {
            let _ = context.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_millis(500)),
            });
            let _ = app_handle.emit(
                "wgpu-frame-ready",
                serde_json::json!({ "path": loaded_image.path }),
            );
            return Ok(b"WGPU_RENDER".to_vec());
        }

        let final_processed_image = Arc::new(final_processed_image);
        let final_rgba_image = match &*final_processed_image {
            DynamicImage::ImageRgba8(img) => img,
            _ => return Err("Expected Rgba8 image from GPU for encoding".to_string()),
        };

        let raw_bytes: &[u8] = final_rgba_image.as_raw();
        let rgba8_pixels: &[RGBA8] = raw_bytes.as_rgba();

        let img_ref = ImgRef::new(
            rgba8_pixels,
            final_rgba_image.width() as usize,
            final_rgba_image.height() as usize,
        );

        let step_start = std::time::Instant::now();

        let encode_result = Encoder::new(Preset::BaselineFastest)
            .quality(jpeg_quality)
            .fast_color(true)
            .encode_imgref(img_ref);

        match encode_result {
            Ok(jpeg_bytes) => {
                if is_interactive {
                    let (roi_w, roi_h) = final_rgba_image.dimensions();
                    let (rx, ry) = if let Some(r) = pixel_roi {
                        (r.x, r.y)
                    } else {
                        (0, 0)
                    };

                    let mut response = Vec::with_capacity(24 + jpeg_bytes.len());
                    response.extend_from_slice(&rx.to_le_bytes());
                    response.extend_from_slice(&ry.to_le_bytes());
                    response.extend_from_slice(&roi_w.to_le_bytes());
                    response.extend_from_slice(&roi_h.to_le_bytes());
                    response.extend_from_slice(&preview_width.to_le_bytes());
                    response.extend_from_slice(&preview_height.to_le_bytes());
                    response.extend_from_slice(&jpeg_bytes);

                    log::info!(
                        "[process_preview_job] interactive ROI {}x{} encode in {:.2?}, total {:.2?}",
                        roi_w,
                        roi_h,
                        step_start.elapsed(),
                        fn_start.elapsed()
                    );
                    Ok(response)
                } else {
                    let (width, height) = final_rgba_image.dimensions();
                    log::info!(
                        "[process_preview_job] full {}x{} q={} encode in {:.2?}, total {:.2?}",
                        width,
                        height,
                        jpeg_quality,
                        step_start.elapsed(),
                        fn_start.elapsed()
                    );
                    Ok(jpeg_bytes)
                }
            }
            Err(e) => Err(format!("Failed to encode preview: {}", e)),
        }
    } else {
        log::error!(
            "[process_preview_job] processing failed after {:.2?}",
            fn_start.elapsed()
        );
        Err("Processing failed".to_string())
    }
}

fn start_analytics_worker(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let (tx, rx): (Sender<AnalyticsJob>, Receiver<AnalyticsJob>) = mpsc::channel();
    *state.analytics_worker_tx.lock().unwrap() = Some(tx);

    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            while let Ok(latest) = rx.try_recv() {
                job = latest;
            }

            let histogram_data = image_processing::calculate_histogram_from_image(&job.image).ok();

            let waveform_data = if job.compute_waveform {
                image_processing::calculate_waveform_from_image(
                    &job.image,
                    job.active_waveform_channel.as_deref(),
                )
                .ok()
            } else {
                None
            };

            if histogram_data.is_some() || waveform_data.is_some() {
                // Per window rather than broadcast: a broadcast gives up at the
                // first window that will not take it, and the order is a hash
                // map's. See resilient_emit.
                crate::resilient_emit::emit_to_every_window(
                    &app_handle,
                    "analytics-update",
                    serde_json::json!({
                        "path": job.path,
                        "histogram": histogram_data,
                        "waveform": waveform_data,
                    }),
                );
            }
        }
    });
}

fn start_preview_worker(app_handle: tauri::AppHandle) {
    let state = app_handle.state::<AppState>();
    let (tx, rx): (Sender<PreviewJob>, Receiver<PreviewJob>) = mpsc::channel();

    *state.preview_worker_tx.lock().unwrap() = Some(tx);

    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            while let Ok(latest_job) = rx.try_recv() {
                job = latest_job;
            }

            let state = app_handle.state::<AppState>();
            let responder = job.responder;
            match process_preview_job(
                &app_handle,
                state,
                job.adjustments,
                job.is_interactive,
                job.target_resolution,
                job.roi,
                job.compute_waveform,
                job.active_waveform_channel.as_deref(),
            ) {
                Ok(bytes) => {
                    let _ = responder.send(bytes);
                }
                // BLITZRAW: still decoding is not an error. See
                // NOTHING_LOADED_YET. Kept visible, since a missing log line is
                // its own kind of confusion, but named for what it is.
                Err(e) if e == NOTHING_LOADED_YET => {
                    log::info!("Preview skipped: the photo is still being decoded");
                }
                Err(e) => {
                    log::error!("Preview worker error: {}", e);
                }
            }
        }
    });
}

#[tauri::command]
async fn apply_adjustments(
    js_adjustments: serde_json::Value,
    is_interactive: bool,
    target_resolution: Option<u32>,
    roi: Option<(f32, f32, f32, f32)>,
    compute_waveform: bool,
    active_waveform_channel: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<Response, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();

    {
        let tx_guard = state.preview_worker_tx.lock().unwrap();
        if let Some(worker_tx) = &*tx_guard {
            let job = PreviewJob {
                adjustments: js_adjustments,
                is_interactive,
                target_resolution,
                roi,
                compute_waveform,
                active_waveform_channel,
                responder: tx,
            };
            worker_tx
                .send(job)
                .map_err(|e| format!("Failed to send to preview worker: {}", e))?;
        } else {
            return Err("Preview worker not running".to_string());
        }
    }

    match rx.await {
        Ok(bytes) => Ok(Response::new(bytes)),
        Err(_) => Err("Superseded or worker failed".to_string()),
    }
}

#[tauri::command]
fn generate_uncropped_preview(
    js_adjustments: serde_json::Value,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let mut adjustments_clone = js_adjustments.clone();
    hydrate_adjustments(&state, &mut adjustments_clone);

    let loaded_image = state
        .original_image
        .lock()
        .unwrap()
        .clone()
        .ok_or("No original image loaded")?;

    thread::spawn(move || {
        let state = app_handle.state::<AppState>();
        let path = loaded_image.path.clone();
        let is_raw = loaded_image.is_raw;
        let unique_hash = calculate_full_job_hash(&path, &adjustments_clone);
        let has_patches = adjustments_clone
            .get("aiPatches")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        let patched_image = if has_patches {
            Cow::Owned(
                composite_patches_on_image(&loaded_image.image, &adjustments_clone).unwrap_or_else(
                    |e| {
                        eprintln!("Failed to composite patches for uncropped preview: {}", e);
                        loaded_image.image.as_ref().clone()
                    },
                ),
            )
        } else {
            Cow::Borrowed(loaded_image.image.as_ref())
        };

        let warped_image = apply_geometry_warp(patched_image, &adjustments_clone);
        let blurred_image = crate::lens_blur::apply_lens_blur(warped_image, &adjustments_clone);
        let orientation_steps = adjustments_clone["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let coarse_rotated_image = apply_coarse_rotation(blurred_image, orientation_steps);

        let flip_horizontal = adjustments_clone["flipHorizontal"]
            .as_bool()
            .unwrap_or(false);
        let flip_vertical = adjustments_clone["flipVertical"].as_bool().unwrap_or(false);

        let flipped_image =
            apply_flip(coarse_rotated_image, flip_horizontal, flip_vertical).into_owned();

        let settings = load_settings(app_handle.clone()).unwrap_or_default();
        let preview_dim = settings.editor_preview_resolution.unwrap_or(1920);

        let (rotated_w, rotated_h) = flipped_image.dimensions();

        let (processing_base, scale_for_gpu) = if rotated_w > preview_dim || rotated_h > preview_dim
        {
            let base = downscale_f32_image(&flipped_image, preview_dim, preview_dim);
            let scale = if rotated_w > 0 {
                base.width() as f32 / rotated_w as f32
            } else {
                1.0
            };
            (base, scale)
        } else {
            (flipped_image.clone(), 1.0)
        };

        let (preview_width, preview_height) = processing_base.dimensions();

        let mask_definitions: Vec<MaskDefinition> = adjustments_clone
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let mask_bitmaps: Vec<ImageBuffer<Luma<u8>, Vec<u8>>> = mask_definitions
            .iter()
            .filter_map(|def| {
                get_cached_or_generate_mask(
                    &state,
                    def,
                    preview_width,
                    preview_height,
                    scale_for_gpu,
                    (0.0, 0.0),
                    &adjustments_clone,
                )
            })
            .collect();

        let tm_override = resolve_tonemapper_override_from_handle(&app_handle, is_raw);
        let mut uncropped_adjustments =
            get_all_adjustments_from_json(&adjustments_clone, is_raw, tm_override);
        // BLITZRAW: white balance from the camera's own calibration.
        crate::image_processing::apply_camera_profile_to_adjustments(
            &mut uncropped_adjustments,
            &path,
            &adjustments_clone,
        );
        let lut_path = adjustments_clone["lutPath"].as_str();
        let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());

        if let Ok(processed_image) = process_and_get_dynamic_image(
            &context,
            &state,
            &processing_base,
            unique_hash,
            RenderRequest {
                adjustments: uncropped_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "generate_uncropped_preview",
        ) {
            let (width, height) = processed_image.dimensions();
            let rgb_pixels = processed_image.to_rgb8().into_vec();
            match Encoder::new(Preset::BaselineFastest)
                .quality(80)
                .encode_rgb(&rgb_pixels, width, height)
            {
                Ok(bytes) => {
                    let base64_str = general_purpose::STANDARD.encode(&bytes);
                    let data_url = format!("data:image/jpeg;base64,{}", base64_str);
                    let _ = app_handle.emit("preview-update-uncropped", data_url);
                }
                Err(e) => {
                    log::error!("Failed to encode uncropped preview with mozjpeg-rs: {}", e);
                }
            }
        }
    });

    Ok(())
}

#[tauri::command]
fn generate_original_transformed_preview(
    js_adjustments: serde_json::Value,
    target_resolution: Option<u32>,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let loaded_image = state
        .original_image
        .lock()
        .unwrap()
        .clone()
        .ok_or("No original image loaded")?;

    let mut adjustments_clone = js_adjustments.clone();

    if let Some(obj) = adjustments_clone.as_object_mut() {
        obj.insert(
            "lensBlurEnabled".to_string(),
            serde_json::Value::Bool(false),
        );
    }

    hydrate_adjustments(&state, &mut adjustments_clone);

    let mut image_for_preview = loaded_image.image.as_ref().clone();
    if loaded_image.is_raw {
        apply_cpu_default_raw_processing(&mut image_for_preview);
    }

    let (transformed_full_res, _unscaled_crop_offset) =
        apply_all_transformations(Cow::Borrowed(&image_for_preview), &adjustments_clone);

    let settings = load_settings(app_handle).unwrap_or_default();
    let default_dim = settings.editor_preview_resolution.unwrap_or(1920);
    let preview_dim = target_resolution.unwrap_or(default_dim);

    let (w, h) = transformed_full_res.dimensions();
    let transformed_image = if w > preview_dim || h > preview_dim {
        downscale_f32_image(transformed_full_res.as_ref(), preview_dim, preview_dim)
    } else {
        transformed_full_res.into_owned()
    };

    let (width, height) = transformed_image.dimensions();
    let rgb_pixels = transformed_image.to_rgb8().into_vec();

    let bytes = Encoder::new(Preset::BaselineFastest)
        .quality(80)
        .encode_rgb(&rgb_pixels, width, height)
        .map_err(|e| format!("Failed to encode with mozjpeg-rs: {}", e))?;

    let base64_str = general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:image/jpeg;base64,{}", base64_str))
}

#[tauri::command]
async fn preview_geometry_transform(
    params: GeometryParams,
    js_adjustments: serde_json::Value,
    show_lines: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let (loaded_image_path, is_raw) = {
        let guard = state.original_image.lock().unwrap();
        let loaded = guard.as_ref().ok_or("No image loaded")?;
        (loaded.path.clone(), loaded.is_raw)
    };

    let visual_hash = calculate_visual_hash(&loaded_image_path, &js_adjustments);

    let base_image_to_warp = {
        let maybe_cached_image = state
            .geometry_cache
            .lock()
            .unwrap()
            .get(&visual_hash)
            .cloned();

        if let Some(cached_image) = maybe_cached_image {
            cached_image
        } else {
            let context = get_or_init_gpu_context(&state, &app_handle)?;

            let original_image = {
                let guard = state.original_image.lock().unwrap();
                let loaded = guard.as_ref().ok_or("No image loaded")?;
                loaded.image.clone()
            };

            let settings = load_settings(app_handle.clone()).unwrap_or_default();
            let interactive_divisor = 1.5;
            let final_preview_dim = settings.editor_preview_resolution.unwrap_or(1920);
            let target_dim = (final_preview_dim as f32 / interactive_divisor) as u32;

            let preview_base = tokio::task::spawn_blocking(move || -> DynamicImage {
                downscale_f32_image(&original_image, target_dim, target_dim)
            })
            .await
            .map_err(|e| e.to_string())?;

            let mut temp_adjustments = js_adjustments.clone();
            hydrate_adjustments(&state, &mut temp_adjustments);

            if let Some(obj) = temp_adjustments.as_object_mut() {
                obj.insert("crop".to_string(), serde_json::Value::Null);
                obj.insert("rotation".to_string(), serde_json::json!(0.0));
                obj.insert("orientationSteps".to_string(), serde_json::json!(0));
                obj.insert("flipHorizontal".to_string(), serde_json::json!(false));
                obj.insert("flipVertical".to_string(), serde_json::json!(false));
                obj.insert("lensBlurEnabled".to_string(), serde_json::json!(false));
                for key in GEOMETRY_KEYS {
                    match *key {
                        "transformScale"
                        | "lensDistortionAmount"
                        | "lensVignetteAmount"
                        | "lensTcaAmount" => {
                            obj.insert(key.to_string(), serde_json::json!(100.0));
                        }
                        "lensDistortionParams" | "lensMaker" | "lensModel" => {
                            obj.insert(key.to_string(), serde_json::Value::Null);
                        }
                        "lensDistortionEnabled" | "lensTcaEnabled" | "lensVignetteEnabled" => {
                            obj.insert(key.to_string(), serde_json::json!(true));
                        }
                        _ => {
                            obj.insert(key.to_string(), serde_json::json!(0.0));
                        }
                    }
                }
            }

            let tm_override = resolve_tonemapper_override_from_handle(&app_handle, is_raw);
            let mut all_adjustments =
                get_all_adjustments_from_json(&temp_adjustments, is_raw, tm_override);
            // BLITZRAW: white balance from the camera's own calibration.
            crate::image_processing::apply_camera_profile_to_adjustments(
                &mut all_adjustments,
                &loaded_image_path,
                &temp_adjustments,
            );
            let lut_path = temp_adjustments["lutPath"].as_str();
            let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());
            let mask_bitmaps = Vec::new();

            let processed_base = process_and_get_dynamic_image(
                &context,
                &state,
                &preview_base,
                visual_hash,
                RenderRequest {
                    adjustments: all_adjustments,
                    mask_bitmaps: &mask_bitmaps,
                    lut,
                    roi: None,
                },
                "preview_geometry_transform_base_gen",
            )?;

            let mut cache = state.geometry_cache.lock().unwrap();
            if cache.len() > 5 {
                cache.clear();
            }
            cache.insert(visual_hash, processed_base.clone());

            processed_base
        }
    };

    let final_image = tokio::task::spawn_blocking(move || -> DynamicImage {
        let mut adjusted_params = params;

        if is_raw {
            // approximate linear vignetting correction on gamma-baked & tonemapped geometry preview
            adjusted_params.lens_vignette_amount *= 0.4;
        } else {
            adjusted_params.lens_vignette_amount *= 0.8;
        }

        let warped_image = warp_image_geometry(&base_image_to_warp, adjusted_params);
        let orientation_steps = js_adjustments["orientationSteps"].as_u64().unwrap_or(0) as u8;
        let flip_horizontal = js_adjustments["flipHorizontal"].as_bool().unwrap_or(false);
        let flip_vertical = js_adjustments["flipVertical"].as_bool().unwrap_or(false);

        let coarse_rotated_image =
            apply_coarse_rotation(Cow::Owned(warped_image), orientation_steps);
        let flipped_image =
            apply_flip(coarse_rotated_image, flip_horizontal, flip_vertical).into_owned();

        if show_lines {
            let gray_image = flipped_image.to_luma8();
            let mut visualization = flipped_image.to_rgba8();
            let edges = canny(&gray_image, 50.0, 100.0);

            let min_dim = gray_image.width().min(gray_image.height());

            let options = LineDetectionOptions {
                vote_threshold: (min_dim as f32 * 0.24) as u32,
                suppression_radius: 15,
            };

            let lines = detect_lines(&edges, options);

            for line in lines {
                let angle_deg = line.angle_in_degrees as f32;
                let angle_norm = angle_deg % 180.0;
                let alignment_threshold = 0.5;
                let is_vertical =
                    angle_norm < alignment_threshold || angle_norm > (180.0 - alignment_threshold);
                let is_horizontal = (angle_norm - 90.0).abs() < alignment_threshold;

                let color = if is_vertical || is_horizontal {
                    Rgba([0, 255, 0, 255])
                } else {
                    Rgba([255, 0, 0, 255])
                };

                let r = line.r;
                let theta_rad = angle_deg.to_radians();
                let a = theta_rad.cos();
                let b = theta_rad.sin();
                let x0 = a * r;
                let y0 = b * r;

                let dist = (visualization.width().max(visualization.height()) * 2) as f32;

                let x1 = x0 + dist * (-b);
                let y1 = y0 + dist * (a);
                let x2 = x0 - dist * (-b);
                let y2 = y0 - dist * (a);

                draw_line_segment_mut(&mut visualization, (x1, y1), (x2, y2), color);
                draw_line_segment_mut(
                    &mut visualization,
                    (x1 + a, y1 + b),
                    (x2 + a, y2 + b),
                    color,
                );
            }

            DynamicImage::ImageRgba8(visualization)
        } else {
            flipped_image
        }
    })
    .await
    .map_err(|e| e.to_string())?;

    let (width, height) = final_image.dimensions();
    let rgb_pixels = final_image.to_rgb8().into_vec();

    let bytes = Encoder::new(Preset::BaselineFastest)
        .quality(75)
        .encode_rgb(&rgb_pixels, width, height)
        .map_err(|e| format!("Failed to encode with mozjpeg-rs: {}", e))?;

    let base64_str = general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:image/jpeg;base64,{}", base64_str))
}

pub fn get_original_image(
    state: &tauri::State<AppState>,
) -> Result<(std::sync::Arc<image::DynamicImage>, bool), String> {
    let original_image_lock = state.original_image.lock().unwrap();
    let loaded_image = original_image_lock
        .as_ref()
        .ok_or("No original image loaded")?;
    Ok((
        std::sync::Arc::clone(&loaded_image.image),
        loaded_image.is_raw,
    ))
}

#[tauri::command]
fn generate_preset_preview(
    js_adjustments: serde_json::Value,
    state: tauri::State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;

    let loaded_image = state
        .original_image
        .lock()
        .unwrap()
        .clone()
        .ok_or("No original image loaded for preset preview")?;
    let is_raw = loaded_image.is_raw;
    let unique_hash = calculate_full_job_hash(&loaded_image.path, &js_adjustments);

    const PRESET_PREVIEW_DIM: u32 = 400;

    let (preview_image, scale_for_gpu, unscaled_crop_offset) =
        generate_transformed_preview(&state, &loaded_image, &js_adjustments, PRESET_PREVIEW_DIM)?;

    let (img_w, img_h) = preview_image.dimensions();

    let mask_definitions: Vec<MaskDefinition> = js_adjustments
        .get("masks")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();

    let scaled_crop_offset = (
        unscaled_crop_offset.0 * scale_for_gpu,
        unscaled_crop_offset.1 * scale_for_gpu,
    );

    let mask_bitmaps: Vec<ImageBuffer<Luma<u8>, Vec<u8>>> = mask_definitions
        .iter()
        .filter_map(|def| {
            get_cached_or_generate_mask(
                &state,
                def,
                img_w,
                img_h,
                scale_for_gpu,
                scaled_crop_offset,
                &js_adjustments,
            )
        })
        .collect();

    let tm_override = resolve_tonemapper_override_from_handle(&app_handle, is_raw);
    let mut all_adjustments = get_all_adjustments_from_json(&js_adjustments, is_raw, tm_override);
    // BLITZRAW: white balance from the camera's own calibration.
    crate::image_processing::apply_camera_profile_to_adjustments(
        &mut all_adjustments,
        &loaded_image.path,
        &js_adjustments,
    );
    let lut_path = js_adjustments["lutPath"].as_str();
    let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());

    let processed_image = process_and_get_dynamic_image(
        &context,
        &state,
        &preview_image,
        unique_hash,
        RenderRequest {
            adjustments: all_adjustments,
            mask_bitmaps: &mask_bitmaps,
            lut,
            roi: None,
        },
        "generate_preset_preview",
    )?;

    let mut buf = Cursor::new(Vec::new());
    processed_image
        .to_rgb8()
        .write_with_encoder(JpegEncoder::new_with_quality(&mut buf, 80))
        .map_err(|e| e.to_string())?;

    Ok(Response::new(buf.into_inner()))
}

#[tauri::command]
async fn fetch_community_presets() -> Result<Vec<CommunityPreset>, String> {
    let client = reqwest::Client::new();
    let url = "https://raw.githubusercontent.com/CyberTimon/RapidRAW-Presets/main/manifest.json";

    let response = client
        .get(url)
        .header("User-Agent", "RapidRAW-App")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch manifest from GitHub: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub returned an error: {}", response.status()));
    }

    let presets: Vec<CommunityPreset> = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse manifest.json: {}", e))?;

    Ok(presets)
}

#[tauri::command]
async fn generate_all_community_previews(
    image_paths: Vec<String>,
    presets: Vec<CommunityPreset>,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let context = get_or_init_gpu_context(&state, &app_handle)?;
    let mut results: HashMap<String, Vec<u8>> = HashMap::new();

    const TILE_DIM: u32 = 360;
    const PROCESSING_DIM: u32 = TILE_DIM * 2;

    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    let mut base_thumbnails: Vec<(DynamicImage, bool, f32)> = Vec::new();
    for image_path in image_paths.iter() {
        let (source_path, _) = parse_virtual_path(image_path);
        let source_path_str = source_path.to_string_lossy().to_string();
        let image_bytes = fs::read(&source_path).map_err(|e| e.to_string())?;
        let original_image = crate::image_loader::load_base_image_from_bytes(
            &image_bytes,
            &source_path_str,
            true,
            &settings,
            None,
        )
        .map_err(|e| e.to_string())?;

        let is_raw = is_raw_file(&source_path_str);
        let (orig_w, orig_h) = original_image.dimensions();
        let (base_image, base_scale) = if orig_w > PROCESSING_DIM || orig_h > PROCESSING_DIM {
            let downscaled = downscale_f32_image(&original_image, PROCESSING_DIM, PROCESSING_DIM);
            let scale = downscaled.width() as f32 / orig_w as f32;
            (downscaled, scale)
        } else {
            (original_image, 1.0)
        };

        base_thumbnails.push((base_image, is_raw, base_scale));
    }

    for preset in presets.iter() {
        let mut processed_tiles: Vec<RgbImage> = Vec::new();
        let js_adjustments = &preset.adjustments;

        let mut preset_hasher = DefaultHasher::new();
        preset.name.hash(&mut preset_hasher);
        let preset_hash = preset_hasher.finish();

        for (i, (base_image, is_raw, base_scale)) in base_thumbnails.iter().enumerate() {
            let mut scaled_adjustments = js_adjustments.clone();
            if let Some(crop_val) = scaled_adjustments.get_mut("crop")
                && let Ok(c) = serde_json::from_value::<Crop>(crop_val.clone())
            {
                *crop_val = serde_json::to_value(Crop {
                    x: c.x * (*base_scale as f64),
                    y: c.y * (*base_scale as f64),
                    width: c.width * (*base_scale as f64),
                    height: c.height * (*base_scale as f64),
                })
                .unwrap_or(serde_json::Value::Null);
            }

            let (transformed_image, _scaled_crop_offset) =
                crate::apply_all_transformations(Cow::Borrowed(base_image), &scaled_adjustments);
            let (img_w, img_h) = transformed_image.dimensions();

            let mask_definitions: Vec<MaskDefinition> = scaled_adjustments
                .get("masks")
                .and_then(|m| serde_json::from_value(m.clone()).ok())
                .unwrap_or_else(Vec::new);

            let unscaled_crop_offset = js_adjustments
                .get("crop")
                .and_then(|c| serde_json::from_value::<Crop>(c.clone()).ok())
                .map_or((0.0, 0.0), |c| (c.x as f32, c.y as f32));
            let actual_scaled_crop_offset = (
                unscaled_crop_offset.0 * base_scale,
                unscaled_crop_offset.1 * base_scale,
            );

            let mask_bitmaps: Vec<ImageBuffer<Luma<u8>, Vec<u8>>> = mask_definitions
                .iter()
                .filter_map(|def| {
                    generate_mask_bitmap(
                        def,
                        img_w,
                        img_h,
                        *base_scale,
                        actual_scaled_crop_offset,
                        None,
                    )
                })
                .collect();

            let tm_override = resolve_tonemapper_override_from_handle(&app_handle, *is_raw);
            let all_adjustments =
                get_all_adjustments_from_json(&scaled_adjustments, *is_raw, tm_override);
            let lut_path = js_adjustments["lutPath"].as_str();
            let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());

            let unique_hash = preset_hash.wrapping_add(i as u64);

            let processed_image_dynamic = crate::image_processing::process_and_get_dynamic_image(
                &context,
                &state,
                transformed_image.as_ref(),
                unique_hash,
                RenderRequest {
                    adjustments: all_adjustments,
                    mask_bitmaps: &mask_bitmaps,
                    lut,
                    roi: None,
                },
                "generate_all_community_previews",
            )?;

            let processed_image = processed_image_dynamic.to_rgb8();

            let (proc_w, proc_h) = processed_image.dimensions();
            let size = proc_w.min(proc_h);
            let cropped_processed_image = image::imageops::crop_imm(
                &processed_image,
                (proc_w - size) / 2,
                (proc_h - size) / 2,
                size,
                size,
            )
            .to_image();

            let final_tile = image::imageops::resize(
                &cropped_processed_image,
                TILE_DIM,
                TILE_DIM,
                image::imageops::FilterType::Lanczos3,
            );
            processed_tiles.push(final_tile);
        }

        let final_image_buffer = match processed_tiles.len() {
            1 => processed_tiles.remove(0),
            2 => {
                let mut canvas = RgbImage::new(TILE_DIM * 2, TILE_DIM);
                image::imageops::overlay(&mut canvas, &processed_tiles[0], 0, 0);
                image::imageops::overlay(&mut canvas, &processed_tiles[1], TILE_DIM as i64, 0);
                canvas
            }
            4 => {
                let mut canvas = RgbImage::new(TILE_DIM * 2, TILE_DIM * 2);
                image::imageops::overlay(&mut canvas, &processed_tiles[0], 0, 0);
                image::imageops::overlay(&mut canvas, &processed_tiles[1], TILE_DIM as i64, 0);
                image::imageops::overlay(&mut canvas, &processed_tiles[2], 0, TILE_DIM as i64);
                image::imageops::overlay(
                    &mut canvas,
                    &processed_tiles[3],
                    TILE_DIM as i64,
                    TILE_DIM as i64,
                );
                canvas
            }
            _ => continue,
        };

        let mut buf = Cursor::new(Vec::new());
        if final_image_buffer
            .write_with_encoder(JpegEncoder::new_with_quality(&mut buf, 75))
            .is_ok()
        {
            results.insert(preset.name.clone(), buf.into_inner());
        }
    }

    Ok(results)
}

#[tauri::command]
async fn save_temp_file(bytes: Vec<u8>) -> Result<String, String> {
    let mut temp_file = NamedTempFile::new().map_err(|e| e.to_string())?;
    temp_file.write_all(&bytes).map_err(|e| e.to_string())?;
    let (_file, path) = temp_file.keep().map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn merge_hdr(
    paths: Vec<String>,
    // BLITZRAW: absent means yes, so a single merge from the modal is
    // unchanged and only the bulk queue has to say anything.
    with_preview: Option<bool>,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if paths.len() < 2 {
        return Err("Please select at least two images to merge.".to_string());
    }

    let hdr_result_handle = state.hdr_result.clone();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();

    // ============== BLITZRAW: what each stage of a merge costs ==============
    // A bulk run is minutes per dozen brackets and the log said only that a
    // merge had started and finished, so which stage owned the time had to be
    // worked out from file modification times afterwards. Twice.
    let began = std::time::Instant::now();
    let stage = std::time::Instant::now();
    let mut frames = load_hdr_frames(&paths, &app_handle, &settings)?;
    let decoded_in = stage.elapsed();
    assert_uniform_dimensions(&frames)?;
    let stage = std::time::Instant::now();
    if settings.hdr_auto_align.unwrap_or(false) {
        align_hdr_frames(&mut frames, &app_handle);
    } else {
        log::info!("HDR alignment disabled by setting, merging frames as shot");
    }
    let aligned_in = stage.elapsed();

    // ========== BLITZRAW: a clipped pixel is not a measurement ==========
    // `image_hdr::hdr_merge_images` sums every frame's contribution with no
    // regard for whether a pixel was blown out in it. That is the estimator
    // from the paper it cites with the paper's own condition left off: the sum
    // runs over the observations that are not saturated. A clipped pixel says
    // "at least this bright" and nothing else, so including it drags the
    // highlight down, and by different amounts per channel, which is what put a
    // yellow cast on bright objects whenever the longest frame was overexposed.
    //
    // Ours does the same arithmetic with the condition put back. See hdr_merge.
    log::info!("Starting HDR merge of {} images", frames.len());
    let stage = std::time::Instant::now();
    let merged = crate::hdr_merge::merge_loaded(&frames);
    let merged_in = stage.elapsed();
    let stage = std::time::Instant::now();
    // And scaled by what the metered frame called white, rather than by whatever
    // happened to be brightest in shot. `apply_histogram_stretch` divides by the
    // largest value in the image, which was survivable only while the merge
    // crushed its own highlights: once they are right, one clipped lamp at a
    // hundred times the brightness of the room divides the room by a hundred,
    // and the corrected merge would have looked far worse than the broken one.
    // See hdr_merge.
    let white = crate::hdr_merge::metered_white_of(&frames);
    log::info!("Merged white point is a radiance of {white:.4}");
    let mut hdr_merged = DynamicImage::ImageRgb32F(crate::hdr_merge::to_display(&merged, white));
    // ======== BLITZRAW END: a clipped pixel is not a measurement ========
    // Everything downstream is display-referred because of this line. The DNG
    // writer says so in a LinearizationTable; see hdr_dng.
    hdr_merged = apply_linear_to_srgb(hdr_merged);
    let toned_in = stage.elapsed();

    // A preview only the modal ever looks at.
    //
    // `merge_hdr` built a full-resolution PNG of the merge and base64'd it into
    // an event whatever the caller was, and the listener that receives it drops
    // the payload outright while a bulk run is going. So a queue of thirty
    // brackets spent about a second each encoding an eighty megabyte string,
    // pushed all 2.4 GB of it across into the webview, and threw every one away
    // on arrival. The queue awaits the command rather than the event, so it
    // asks for no preview and nothing downstream notices.
    let stage = std::time::Instant::now();
    let final_base64 = if with_preview.unwrap_or(true) {
        let mut buf = Cursor::new(Vec::new());
        if let Err(e) = hdr_merged.to_rgb8().write_to(&mut buf, ImageFormat::Png) {
            return Err(format!("Failed to encode hdr preview: {}", e));
        }
        Some(format!(
            "data:image/png;base64,{}",
            general_purpose::STANDARD.encode(buf.get_ref())
        ))
    } else {
        None
    };
    log::info!(
        "HDR merge of {} took {:?}: decode {:?}, align {:?}, merge {:?}, tone {:?}, preview {:?} ({})",
        paths.len(),
        began.elapsed(),
        decoded_in,
        aligned_in,
        merged_in,
        toned_in,
        stage.elapsed(),
        match &final_base64 {
            Some(b64) => format!("{:.0} MB of base64", b64.len() as f64 / 1e6),
            None => "not asked for".to_string(),
        }
    );
    // ============ BLITZRAW END: what each stage of a merge costs ============

    let _ = app_handle.emit("hdr-progress", "Creating preview...");

    *hdr_result_handle.lock().unwrap() = Some(hdr_merged);

    // Still announced when there is no preview, so anything waiting on the
    // event rather than on the command still hears that the merge is done.
    let _ = app_handle.emit(
        "hdr-complete",
        serde_json::json!({
            "base64": final_base64,
        }),
    );
    Ok(())
}

#[tauri::command]
async fn save_hdr(
    first_path_str: String,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let hdr_image = state.hdr_result.lock().unwrap().take().ok_or_else(|| {
        "No hdr image found in memory to save. It might have already been saved.".to_string()
    })?;

    let (first_path, _) = parse_virtual_path(&first_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory of the first image.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("hdr");

    // ================= BLITZRAW: merges as JPEG XL DNGs =================
    // The float branch is the real merge, and it used to be written as an
    // uncompressed 32-bit float TIFF: 545 MB for a Z9 bracket, which is
    // exactly the pixels with a header on the front. It is a linear DNG with
    // JPEG XL pixels now, about 20 MB for the same picture, measured. See
    // hdr_dng for why that is possible without forking anything and what the
    // quality costs, which is half a display level.
    //
    // The other two branches are for merges that are not float, which means
    // they did not come from raw files, and they keep the format they had.
    let output_path = if hdr_image.as_rgb32f().is_some() && !hdr_image.color().has_alpha() {
        let path = parent_dir.join(format!("{}_Hdr.dng", stem));
        let stage = std::time::Instant::now();
        crate::hdr_dng::write_linear_jxl_dng(&path, &hdr_image, crate::hdr_dng::DEFAULT_DISTANCE)
            .map_err(|e| format!("Failed to save hdr image: {}", e))?;
        log::info!(
            "Wrote {} in {:?} ({:.1} MB)",
            path.display(),
            stage.elapsed(),
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1e6
        );
        path
    } else {
        let (output_filename, image_to_save): (String, DynamicImage) =
            if hdr_image.color().has_alpha() {
                (
                    format!("{}_Hdr.png", stem),
                    DynamicImage::ImageRgba8(hdr_image.to_rgba8()),
                )
            } else {
                (
                    format!("{}_Hdr.png", stem),
                    DynamicImage::ImageRgb8(hdr_image.to_rgb8()),
                )
            };
        let path = parent_dir.join(output_filename);
        image_to_save
            .save(&path)
            .map_err(|e| format!("Failed to save hdr image: {}", e))?;
        path
    };
    // =============== BLITZRAW END: merges as JPEG XL DNGs ===============

    let (real_path, _) = crate::file_management::parse_virtual_path(&first_path_str);
    let _ =
        crate::exif_processing::write_rrexif_sidecar(&real_path.to_string_lossy(), &output_path);

    // ============== BLITZRAW: real white balance, in Kelvin ==============
    // The merge output is a TIFF and carries no camera calibration of its own,
    // so without this it falls back to the old relative tint and the Kelvin
    // slider disappears the moment you merge a bracket.
    //
    // Its pixels are the as-shot render of frames that did have a calibration,
    // and the merge is linear in them: the exposures are combined in linear
    // light and the histogram stretch that follows is one scale and offset
    // applied to every channel alike. So the same white balance matrix applies
    // to the merged file exactly as it did to its inputs, provided the profile
    // travels with it. Nothing here is approximated.
    crate::camera_profile::inherit_profile(&real_path.to_string_lossy(), &output_path);
    // ============ BLITZRAW END: real white balance, in Kelvin ============

    // Last, because both caches key on the written file's modification time and
    // on the sidecar beside it, and everything above still changes those.
    warm_caches_for_merge(hdr_image, &output_path, &app_handle, &state);

    Ok(output_path.to_string_lossy().to_string())
}

// ========== BLITZRAW: render a merge's previews while it is in memory ==========
/// Renders what a merge is about to be asked for, before it is asked.
///
/// A merge is written and then looked at straight away, so the library asks for
/// a thumbnail and the editor asks for a preview, and each of those was a fresh
/// decode of the file just written: about 1.6s apiece on a 45 megapixel DNG,
/// paid twice per bracket on top of a merge that already cost twenty seconds.
/// The picture is still in memory here, so both come from it and neither costs
/// a decode.
///
/// A preloaded image has to be in the space the decode would have produced, or
/// the render is wrong in exactly the way every merge was wrong before the
/// linearization table, and cached that way. `hdr_dng::as_decoded` is what puts
/// it there, and is checked against a real round trip by its own test.
///
/// Both caches key on the written file and its sidecar in the ordinary way, so
/// what is left here is found by an ordinary lookup. Nothing downstream needs
/// to know a merge put it there.
///
/// Every failure is logged and swallowed. A cache that was not warmed costs a
/// decode later, which is what happened every time before this.
fn warm_caches_for_merge(
    merged: DynamicImage,
    output_path: &std::path::Path,
    app_handle: &tauri::AppHandle,
    state: &tauri::State<'_, AppState>,
) {
    let began = std::time::Instant::now();
    let path_str = output_path.to_string_lossy().to_string();
    let settings = load_settings(app_handle.clone()).unwrap_or_default();
    let preview_width = settings
        .editor_preview_resolution
        .unwrap_or(1920)
        .clamp(320, 8192);

    let preloaded = crate::hdr_dng::as_decoded(merged, output_path);

    let gpu_context = crate::gpu_processing::get_or_init_gpu_context(state, app_handle).ok();
    let rendered = match crate::file_management::generate_thumbnail_data(
        &path_str,
        gpu_context.as_ref(),
        Some(&preloaded),
        app_handle,
        Some(preview_width),
    ) {
        Ok(image) => image,
        Err(e) => {
            log::warn!("Could not render the previews for {path_str}: {e}");
            return;
        }
    };
    // The full-size copy has done its job and is the largest thing here.
    drop(preloaded);

    let mut stored = Vec::new();
    match crate::preview_cache::store_rendered_preview(&path_str, &rendered, preview_width) {
        Ok(_) => stored.push(format!("{preview_width}px preview")),
        Err(e) => log::warn!("Could not store the preview for {path_str}: {e}"),
    }
    match crate::file_management::store_rendered_thumbnail(&path_str, &rendered, app_handle) {
        Ok(written) => {
            stored.push("thumbnail".to_string());
            // BLITZRAW: and say so.
            //
            // Writing the file is not enough. A thumbnail keeps one name for
            // the life of the photo, so the address the grid is already showing
            // does not change when the file behind it does, and the front end
            // only reaches for a new one when it is told. Merging a bracket a
            // second time over an existing result therefore wrote a correct new
            // thumbnail that nobody ever looked at, and the grid kept the old
            // picture until the app was restarted.
            crate::file_management::announce_new_thumbnail(app_handle, &path_str, &written);
        }
        Err(e) => log::warn!("Could not store the thumbnail for {path_str}: {e}"),
    }

    if stored.is_empty() {
        log::warn!("Warmed nothing for {path_str}");
    } else {
        log::info!(
            "Warmed the {} for {} in {:?}",
            stored.join(" and the "),
            path_str,
            began.elapsed()
        );
    }
}
// ======== BLITZRAW END: render a merge's previews while it is in memory ========

#[tauri::command]
async fn save_collage(base64_data: String, first_path_str: String) -> Result<String, String> {
    let data_url_prefix = "data:image/png;base64,";
    if !base64_data.starts_with(data_url_prefix) {
        return Err("Invalid base64 data format".to_string());
    }
    let encoded_data = &base64_data[data_url_prefix.len()..];

    let decoded_bytes = general_purpose::STANDARD
        .decode(encoded_data)
        .map_err(|e| format!("Failed to decode base64: {}", e))?;

    let (first_path, _) = parse_virtual_path(&first_path_str);
    let parent_dir = first_path
        .parent()
        .ok_or_else(|| "Could not determine parent directory of the first image.".to_string())?;
    let stem = first_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("collage");

    let output_filename = format!("{}_Collage.png", stem);
    let output_path = parent_dir.join(output_filename);

    fs::write(&output_path, &decoded_bytes)
        .map_err(|e| format!("Failed to save collage image: {}", e))?;

    Ok(output_path.to_string_lossy().to_string())
}

#[tauri::command]
async fn generate_preview_for_path(
    path: String,
    js_adjustments: Value,
    app_handle: tauri::AppHandle,
) -> Result<Response, String> {
    tokio::task::spawn_blocking(move || {
        let state = app_handle.state::<AppState>();
        let context = get_or_init_gpu_context(&state, &app_handle)?;
        let (source_path, _) = parse_virtual_path(&path);
        let source_path_str = source_path.to_string_lossy().to_string();
        let is_raw = is_raw_file(&source_path_str);
        let settings = load_settings(app_handle.clone()).unwrap_or_default();

        let base_image = match read_file_mapped(&source_path) {
            Ok(mmap) => load_and_composite(
                &mmap,
                &source_path_str,
                &js_adjustments,
                false,
                &settings,
                None,
            )
            .map_err(|e| e.to_string())?,
            Err(e) => {
                log::warn!(
                    "Failed to memory-map file '{}': {}. Falling back to standard read.",
                    source_path_str,
                    e
                );
                let bytes = fs::read(&source_path).map_err(|io_err| io_err.to_string())?;
                load_and_composite(
                    &bytes,
                    &source_path_str,
                    &js_adjustments,
                    false,
                    &settings,
                    None,
                )
                .map_err(|e| e.to_string())?
            }
        };

        let (transformed_image, unscaled_crop_offset) =
            apply_all_transformations(Cow::Borrowed(&base_image), &js_adjustments);
        let (img_w, img_h) = transformed_image.dimensions();
        let mask_definitions: Vec<MaskDefinition> = js_adjustments
            .get("masks")
            .and_then(|m| serde_json::from_value(m.clone()).ok())
            .unwrap_or_default();

        let warped_image =
            resolve_warped_image_for_masks(&state, &js_adjustments, &mask_definitions);
        let mask_bitmaps: Vec<ImageBuffer<Luma<u8>, Vec<u8>>> = mask_definitions
            .iter()
            .filter_map(|def| {
                generate_mask_bitmap(
                    def,
                    img_w,
                    img_h,
                    1.0,
                    unscaled_crop_offset,
                    warped_image.as_deref(),
                )
            })
            .collect();

        let tm_override = resolve_tonemapper_override(&settings, is_raw);
        let all_adjustments = get_all_adjustments_from_json(&js_adjustments, is_raw, tm_override);
        let lut_path = js_adjustments["lutPath"].as_str();
        let lut = lut_path.and_then(|p| lut_processing::get_or_load_lut(&state, p).ok());
        let unique_hash = calculate_full_job_hash(&source_path_str, &js_adjustments);

        let final_image = process_and_get_dynamic_image(
            &context,
            &state,
            transformed_image.as_ref(),
            unique_hash,
            RenderRequest {
                adjustments: all_adjustments,
                mask_bitmaps: &mask_bitmaps,
                lut,
                roi: None,
            },
            "generate_preview_for_path",
        )?;

        let (width, height) = final_image.dimensions();
        let rgb_pixels = final_image.to_rgb8().into_vec();

        let bytes = Encoder::new(Preset::BaselineFastest)
            .quality(92)
            .encode_rgb(&rgb_pixels, width, height)
            .map_err(|e| format!("Failed to encode with mozjpeg-rs: {}", e))?;

        Ok(Response::new(bytes))
    })
    .await
    .map_err(|e| format!("Task execution failed: {}", e))?
}

/// Where this run is actually logging, once that is settled.
///
/// Not derived a second time by whoever asks: the file in use is not always
/// `app.log`, and answering with a guess is how the last problem hid.
static LOG_FILE_IN_USE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// How many sessions of logs are kept, including the one running.
const LOGS_KEPT: usize = 10;

/// Every log file this app has written, oldest first.
///
/// Matched by prefix rather than by an exact name, so the numbered files an
/// earlier scheme left behind are pruned by the same rule.
///
/// Ordered by modification time, then by name. Two files written in the same
/// clock tick would otherwise come back in whatever order the directory gave
/// them, and the name carries the start time, so it breaks the tie correctly.
fn log_files_oldest_first(log_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(log_dir) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("app") && name.ends_with(".log"))
        })
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, path)| path).collect()
}

/// Deletes the oldest logs until only `LOGS_KEPT` remain.
///
/// Run after the new file exists, so the session starting is the newest one and
/// cannot delete itself. Failures are ignored: a log that will not delete is
/// clutter, not a reason to start without logging.
fn prune_logs(log_dir: &std::path::Path) {
    let files = log_files_oldest_first(log_dir);
    let excess = files.len().saturating_sub(LOGS_KEPT);
    for path in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}

/// A file of its own for this session, named for when it started.
///
/// One file per run rather than one file reused. Reusing it meant a second
/// instance could not open it at all and fell back to console only, and it
/// meant the run worth reading was erased by the restart that followed it. A
/// name carrying the start time also makes "the log from the merge I ran at
/// nine" something you can pick out of the folder by eye.
fn session_log_path(log_dir: &std::path::Path) -> std::path::PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let plain = log_dir.join(format!("app-{stamp}.log"));
    if plain.exists() {
        // Two instances inside the same second. Rare, and the loser would
        // otherwise truncate the winner's log on the way in.
        return log_dir.join(format!("app-{stamp}-pid{}.log", std::process::id()));
    }
    plain
}

fn setup_logging(app_handle: &tauri::AppHandle) {
    let log_dir = match app_handle.path().app_log_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Failed to get app log directory: {}", e);
            return;
        }
    };

    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!("Failed to create log directory at {:?}: {}", log_dir, e);
    }

    // This session gets a file of its own. Nothing another instance holds can
    // block it, and nothing worth reading is erased by the next restart.
    let log_file_path = session_log_path(&log_dir);
    let log_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&log_file_path)
        .ok();

    if log_file.is_some() {
        prune_logs(&log_dir);
    }

    let var = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let level: log::LevelFilter = var.parse().unwrap_or(log::LevelFilter::Info);

    let writing_to_file = log_file.is_some();
    if writing_to_file {
        let _ = LOG_FILE_IN_USE.set(log_file_path.clone());
    } else {
        eprintln!(
            "Failed to open a log file in {:?}. Logging to console only.",
            log_dir
        );
    }

    // One sink for both the file and the console, on a thread of its own.
    // Chaining stderr directly meant every log call wrote to a pipe from
    // whatever thread made it, and a pipe the dev launcher stops draining
    // blocks the writer. See log_sink.
    let dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        .level(level)
        .chain(Box::new(crate::log_sink::NonBlockingSink::start(log_file))
            as Box<dyn std::io::Write + Send>);

    if let Err(e) = dispatch.apply() {
        eprintln!("Failed to apply logger configuration: {}", e);
    }

    // Reported here because it is the one place that runs after the logger is
    // live and before anything interesting has happened, so a non-zero count
    // means the very start of the run was already too noisy for the console.
    let dropped = crate::log_sink::dropped_records();
    if dropped > 0 {
        log::warn!("{dropped} log records were dropped while the logger was starting");
    }

    if !writing_to_file {
        // Said through the logger as well, so it appears in whatever console
        // is being watched rather than only in stderr at startup.
        log::warn!(
            "No log file could be opened in {:?}. This session is console only.",
            log_dir
        );
    }

    panic::set_hook(Box::new(|info| {
        let message = if let Some(s) = info.payload().downcast_ref::<&'static str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            format!("{:?}", info.payload())
        };
        let location = info.location().map_or_else(
            || "at an unknown location".to_string(),
            |loc| format!("at {}:{}:{}", loc.file(), loc.line(), loc.column()),
        );
        log::error!("PANIC! {} - {}", location, message.trim());
    }));

    // Said here rather than where it is decided. The data directory is settled
    // before the logger exists, because the logger reads settings to know where
    // to write and settings live in the directory being chosen, so that line
    // went to a logger that was not listening. See data_dir.rs.
    crate::data_dir::report_choice(app_handle);

    log::info!(
        "Logger initialized successfully. Log file at: {:?}",
        log_file_path
    );
}

#[tauri::command]
fn get_log_file_path(app_handle: tauri::AppHandle) -> Result<String, String> {
    // Whatever this run settled on, which is not always app.log.
    if let Some(path) = LOG_FILE_IN_USE.get() {
        return Ok(path.to_string_lossy().to_string());
    }
    // No file this session, so offer the most recent one there is rather than
    // a name that may never have existed.
    let log_dir = app_handle.path().app_log_dir().map_err(|e| e.to_string())?;
    match log_files_oldest_first(&log_dir).pop() {
        Some(newest) => Ok(newest.to_string_lossy().to_string()),
        None => Ok(log_dir.to_string_lossy().to_string()),
    }
}

#[tauri::command]
fn frontend_log(level: String, message: String) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let log_line = |line: &str| match level.to_lowercase().as_str() {
        "error" => log::error!("[frontend] {}", line),
        "warn" => log::warn!("[frontend] {}", line),
        "debug" => log::debug!("[frontend] {}", line),
        "trace" => log::trace!("[frontend] {}", line),
        _ => log::info!("[frontend] {}", line),
    };

    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        log_line(line);
    }

    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct MonitorBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

// ============ BLITZRAW: the window that opened small ============
/// Whether what the window is currently reporting about itself should be
/// written down. Three reasons it should not, and each one produced the same
/// symptom: an application that opened small having been closed maximised.
///
/// - **Before the restore.** Setup sizes and positions the window from the file
///   before anything has maximised it, and those calls fire Resized and Moved.
///   Saving then writes `maximized: false` over the answer the restore has not
///   read yet. On a fast start the restore wins the race; on a slow one, which
///   is every start after a rebuild, it does not.
/// - **While closing.** Windows sends a last Resized as a window is destroyed,
///   and a maximised window does not always still report itself as maximised by
///   then.
/// - **While minimised.** A minimised window's size and position are not where
///   the user left it either.
fn window_state_is_worth_saving(restored: bool, closing: bool, minimized: bool) -> bool {
    restored && !closing && !minimized
}

/// Writes whatever window state is pending, now, instead of waiting for the
/// saver's next turn. Called as the window closes: the saver runs every 500 ms
/// and the process does not always last that long.
fn flush_window_state(app_handle: &tauri::AppHandle, pending: &Arc<Mutex<Option<WindowState>>>) {
    let Some(state) = pending.lock().unwrap().take() else {
        return;
    };
    // BLITZRAW: one data directory, chosen and proved. See data_dir.rs.
    let dir = crate::data_dir::data_dir(app_handle);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string(&state) {
        let _ = std::fs::write(dir.join("window_state.json"), json);
    }
}
// ========== BLITZRAW END: the window that opened small ==========

fn saved_window_state_is_usable(state: &WindowState, monitors: &[MonitorBounds]) -> bool {
    if state.width < 800 || state.height < 600 {
        return false;
    }

    if monitors.is_empty() {
        return true;
    }

    let window_left = state.x as i64;
    let window_top = state.y as i64;
    let window_right = window_left + state.width as i64;
    let window_bottom = window_top + state.height as i64;

    monitors.iter().any(|monitor| {
        let monitor_left = monitor.x as i64;
        let monitor_top = monitor.y as i64;
        let monitor_right = monitor_left + monitor.width as i64;
        let monitor_bottom = monitor_top + monitor.height as i64;

        let overlap_width = window_right.min(monitor_right) - window_left.max(monitor_left);
        let overlap_height = window_bottom.min(monitor_bottom) - window_top.max(monitor_top);

        overlap_width >= 100 && overlap_height >= 100
    })
}

#[cfg(not(target_os = "android"))]
fn available_monitor_bounds(window: &tauri::WebviewWindow) -> Vec<MonitorBounds> {
    window
        .available_monitors()
        .map(|monitors| {
            monitors
                .into_iter()
                .map(|monitor| {
                    let position = monitor.position();
                    let size = monitor.size();
                    MonitorBounds {
                        x: position.x,
                        y: position.y,
                        width: size.width,
                        height: size.height,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(target_os = "android")]
fn available_monitor_bounds(_window: &tauri::WebviewWindow) -> Vec<MonitorBounds> {
    Vec::new()
}

#[tauri::command]
fn frontend_ready(
    app_handle: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<AppState>,
) -> Result<LaunchPayload, String> {
    let is_first_run = !state
        .window_setup_complete
        .swap(true, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_os = "android")]
    let _ = (is_first_run, &window, &app_handle);

    #[cfg(not(target_os = "android"))]
    {
        #[cfg(any(windows, target_os = "linux"))]
        let mut should_maximize = false;
        #[cfg(any(windows, target_os = "linux"))]
        let mut should_fullscreen = false;
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = is_first_run;
        // BLITZRAW: the window state no longer comes from a file read here, so
        // on the desktop targets nothing in this function needs the handle. It
        // stays in the signature because Tauri injects it and other targets use
        // it.
        let _ = &app_handle;

        #[cfg(any(windows, target_os = "linux"))]
        // BLITZRAW: the state as it was on disk when the application started,
        // not as it is now. Re-reading the file here is what made the window
        // open small: setup's own set_size and set_position fire Resized and
        // Moved, and the saver had already written a not-yet-maximised window
        // over the answer. A slow start, which is every start after a rebuild,
        // loses the race every time.
        if is_first_run {
            let saved = *state.startup_window_state.lock().unwrap();

            if let Some(saved_state) = saved {
                #[cfg(any(windows, target_os = "linux"))]
                {
                    should_maximize = saved_state.maximized;
                    should_fullscreen = saved_state.fullscreen;
                }

                if (should_maximize || should_fullscreen)
                    && let Some(monitor) = window
                        .current_monitor()
                        .ok()
                        .flatten()
                        .or_else(|| window.primary_monitor().ok().flatten())
                        .or_else(|| {
                            window
                                .available_monitors()
                                .ok()
                                .and_then(|m| m.into_iter().next())
                        })
                {
                    let monitor_size = monitor.size();
                    let monitor_pos = monitor.position();
                    let default_width = 1280i32;
                    let default_height = 720i32;
                    let center_x = monitor_pos.x + (monitor_size.width as i32 - default_width) / 2;
                    let center_y =
                        monitor_pos.y + (monitor_size.height as i32 - default_height) / 2;

                    let _ = window.set_size(tauri::PhysicalSize::new(
                        default_width as u32,
                        default_height as u32,
                    ));
                    let _ = window.set_position(tauri::PhysicalPosition::new(center_x, center_y));
                }
            }
        }

        if let Err(e) = window.show() {
            log::error!("Failed to show window: {}", e);
        }
        if let Err(e) = window.set_focus() {
            log::error!("Failed to focus window: {}", e);
        }
        #[cfg(any(windows, target_os = "linux"))]
        if is_first_run {
            // Reported because a window that opens small when it was closed
            // maximised has three possible explanations, and only the log can
            // say which: the file was not read, it was read and said no, or it
            // said yes and the call did not take.
            log::info!(
                "Window restore: first run, saved state says maximized={should_maximize} fullscreen={should_fullscreen}"
            );
            if should_maximize {
                if let Err(e) = window.maximize() {
                    log::warn!("Could not maximize the window: {e}");
                } else {
                    log::info!("Window maximized, now {:?}", window.is_maximized());
                }
            }
            if should_fullscreen {
                let _ = window.set_fullscreen(true);
            }
            // BLITZRAW: only now is the window where the user left it, so only
            // now is what it reports worth writing down.
            state
                .window_state_restored
                .store(true, std::sync::atomic::Ordering::SeqCst);
        } else {
            log::info!("Window restore skipped: frontend_ready has already run this session");
        }
    }

    let open_with_file = state.initial_file_path.lock().unwrap().take();
    let edit_session = state.pending_edit_session.lock().unwrap().take();
    if let Some(path) = &open_with_file {
        log::info!("Frontend is ready, returning initial path: {}", path);
    }
    if let Some(session) = &edit_session {
        log::info!(
            "Frontend is ready, returning external edit session for: {}",
            &session.source
        );
    }
    Ok(LaunchPayload {
        open_with_file,
        edit_session,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(8 * 1024 * 1024)
        .build_global();

    let mut builder = tauri::Builder::default();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch_req = parse_launch_args(&args);
    let is_headless = matches!(launch_req, LaunchRequest::HeadlessExport(_));

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        if !is_headless {
            builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
                log::info!(
                    "New instance launched with args: {:?}. Focusing main window.",
                    argv
                );
                if let Some(window) = app.get_webview_window("main") {
                    if let Err(e) = window.unminimize() {
                        log::error!("Failed to unminimize window: {}", e);
                    }
                    if let Err(e) = window.set_focus() {
                        log::error!("Failed to set focus on window: {}", e);
                    }
                }

                let forwarded_args = argv.get(1..).unwrap_or(&[]);
                emit_launch_request(app, parse_launch_args(forwarded_args));
            }));
        }
    }

    builder
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(PinchZoomDisablePlugin)
        .on_window_event(|window, event| if let tauri::WindowEvent::Resized(size) = event {
            // ============ BLITZRAW: only the window the picture is in ============
            // This is registered on the builder, so it runs for every window in
            // the application, and the photograph is drawn by a native wgpu
            // surface attached to `main` alone. Without this line, resizing a
            // detached panel window reconfigured the main window's surface to
            // the panel's size: the picture appeared squeezed into a strip of
            // the shape of the other window, and grew and shrank in mirror
            // image as it was dragged. Harmless while there was only ever one
            // window, which is why it was here to be found.
            if window.label() != "main" {
                return;
            }
            // ========== BLITZRAW END: only the window the picture is in ==========
            let state = window.state::<AppState>();
            if let Some(ctx) = state.gpu_context.lock().unwrap().as_ref()
                && let Ok(mut display_lock) = ctx.display.try_lock()
                    && let Some(display) = display_lock.as_mut() {
                        display.config.width = size.width.max(1);
                        display.config.height = size.height.max(1);
                        display.surface.configure(&ctx.device, &display.config);
                        display.render(&ctx.device, &ctx.queue);
                    }
        })
        .setup(move |app| {
            let state = app.state::<AppState>();

            #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
            {
                match launch_req.clone() {
                    LaunchRequest::EditSession(session) => {
                        log::info!("Initial launch with external edit session for: {}", &session.source);
                        *state.pending_edit_session.lock().unwrap() = Some(session);
                    }
                    LaunchRequest::OpenFile(path) => {
                        log::info!("Initial open: Storing path {} for later.", &path);
                        *state.initial_file_path.lock().unwrap() = Some(path);
                    }
                    _ => {}
                }
            }

            let app_handle = app.handle().clone();

            {
                let disks_app_handle = app_handle.clone();
                std::thread::spawn(move || {
                    let disks = sysinfo::Disks::new_with_refreshed_list();
                    let state = disks_app_handle.state::<AppState>();
                    *state.disks_cache.lock().unwrap() = Some(disks);
                });
            }

            // BLITZRAW: one data directory, chosen and proved. See data_dir.rs.
            let config_dir = crate::data_dir::data_dir(&app_handle);
            let crash_flag_path = config_dir.join(".gpu_init_crash_flag");

            {
                let state = app.state::<AppState>();
                *state.gpu_crash_flag_path.lock().unwrap() = Some(crash_flag_path.clone());
            }

            let mut settings: AppSettings = load_settings(app_handle.clone()).unwrap_or_default();

            {
                let state = app.state::<AppState>();
                let cache_size = settings.image_cache_size.unwrap_or(5) as usize;
                state.decoded_image_cache.lock().unwrap().set_capacity(cache_size);
            }

            if crash_flag_path.exists() {
                log::warn!("GPU Driver crash detected on last run! Falling back to OpenGL backend.");
                settings.processing_backend = Some("gl".to_string());
                let _ = crate::save_settings(settings.clone(), app_handle.clone());
                let _ = std::fs::remove_file(&crash_flag_path);
            }

            let lens_db = lens_correction::load_lensfun_db(&app_handle);
            {
                let state = app.state::<AppState>();
                *state.lens_db.lock().unwrap() = Some(Arc::new(lens_db));
            }

            unsafe {
                if let Some(backend) = &settings.processing_backend
                    && backend != "auto" {
                        std::env::set_var("WGPU_BACKEND", backend);
                    }

                #[cfg(target_os = "linux")]
                {
                    apply_workaround_with_options(ApplyWorkaroundOptions::default());
                    if settings.linux_gpu_optimization.unwrap_or(false) {
                        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
                        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
                        std::env::set_var("NODEVICE_SELECT", "1");
                    }
                }

                #[cfg(not(target_os = "android"))]
                {
                    let resource_path = app_handle
                        .path()
                        .resolve("resources", tauri::path::BaseDirectory::Resource)
                        .expect("failed to resolve resource directory");

                    let ort_library_name = {
                        #[cfg(target_os = "windows")]
                        { "onnxruntime.dll" }
                        #[cfg(target_os = "linux")]
                        { "libonnxruntime.so" }
                        #[cfg(target_os = "macos")]
                        { "libonnxruntime.dylib" }
                        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
                        { "libonnxruntime.so" }
                    };
                    let ort_library_path = resource_path.join(ort_library_name);
                    std::env::set_var("ORT_DYLIB_PATH", &ort_library_path);
                    println!("Set ORT_DYLIB_PATH to: {}", ort_library_path.display());

                    // BLITZRAW: the runtime is loaded by full path, but the
                    // DirectML library it needs is looked for the ordinary way,
                    // which does not include the folder the runtime came from.
                    // Without this the load fails with nothing but "the
                    // specified module could not be found", naming the runtime
                    // rather than the file that is actually missing.
                    #[cfg(target_os = "windows")]
                    {
                        let existing = std::env::var("PATH").unwrap_or_default();
                        let resources = resource_path.to_string_lossy().to_string();
                        if !existing.split(';').any(|p| p == resources) {
                            std::env::set_var("PATH", format!("{resources};{existing}"));
                        }
                    }
                }
            }

            setup_logging(&app_handle);

            if let Some(backend) = &settings.processing_backend
                && backend != "auto" {
                    log::info!("Applied processing backend setting: {}", backend);
                }
            #[cfg(target_os = "linux")]
            if settings.linux_gpu_optimization.unwrap_or(false) {
                log::info!("Applied Linux Compatibility Mode (forced software compositing).");
            } else {
                match needs_workaround() {
                    WorkaroundKind::None => {}
                    kind => log::info!("Applied Nvidia workaround: {:?}", kind),
                }
            }

            if let LaunchRequest::HeadlessExport(session) = launch_req {
                let app_handle_clone = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    match crate::export_processing::run_headless_export(session, app_handle_clone.clone()).await {
                        Ok(_) => {
                            println!("Headless export completed successfully.");
                            app_handle_clone.exit(0);
                        }
                        Err(e) => {
                            eprintln!("Headless export failed: {}", e);
                            app_handle_clone.exit(1);
                        }
                    }
                });

                return Ok(());
            }

            start_preview_worker(app_handle.clone());
            start_analytics_worker(app_handle.clone());
            file_management::start_thumbnail_workers(app_handle.clone());
            file_management::start_metadata_workers(app_handle.clone());
            jxl_oxide::integration::register_image_decoding_hook();

            let window_cfg = app.config().app.windows.first().unwrap().clone();
            let decorations = settings.decorations.unwrap_or(window_cfg.decorations);
            #[cfg(target_os = "android")]
            let _ = decorations;

            let main_window_cfg = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .expect("Main window config not found")
                .clone();

            let mut window_builder =
                tauri::WebviewWindowBuilder::from_config(app.handle(), &main_window_cfg)
                    .unwrap();

            #[cfg(not(target_os = "android"))]
            {
                window_builder = window_builder.decorations(decorations).visible(false);
            }

            let window = window_builder.build().expect("Failed to build window");

            #[cfg(target_os = "android")]
            android_integration::initialize_android(&window);

            #[cfg(not(target_os = "android"))]
            {
                let app_state = app.state::<AppState>();
                if let Err(error) = get_or_init_gpu_context(&app_state, app.handle()) {
                    log::warn!(
                        "GPU pre-initialization failed (editing and thumbnails may be degraded): {}",
                        error
                    );
                }

                // BLITZRAW: one data directory, chosen and proved. See data_dir.rs.
                //
                // Read once, and kept, because the calls just below fire Resized
                // and Moved and the saver would write over the answer before
                // `frontend_ready` had a chance to use it. That is the whole of
                // the window-opens-small bug.
                {
                    let path = crate::data_dir::data_path(app.handle(), "window_state.json");
                    let saved = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|contents| serde_json::from_str::<WindowState>(&contents).ok());

                    *app.state::<AppState>().startup_window_state.lock().unwrap() = saved;

                    match saved {
                        Some(state) => {
                            let monitor_bounds = available_monitor_bounds(&window);
                            if saved_window_state_is_usable(&state, &monitor_bounds) {
                                log::info!(
                                    "Window state on disk: {}x{} at {},{} maximized={} fullscreen={}",
                                    state.width,
                                    state.height,
                                    state.x,
                                    state.y,
                                    state.maximized,
                                    state.fullscreen
                                );
                                let _ = window.set_size(tauri::Size::Physical(
                                    tauri::PhysicalSize::new(state.width, state.height),
                                ));
                                let _ = window.set_position(tauri::Position::Physical(
                                    tauri::PhysicalPosition::new(state.x, state.y),
                                ));
                            } else {
                                log::warn!(
                                    "Saved window state was unusable ({}x{} at {},{}), centering instead.",
                                    state.width,
                                    state.height,
                                    state.x,
                                    state.y
                                );
                                let _ = window.center();
                            }
                        }
                        None => {
                            log::info!("No usable window state on disk, centering");
                            let _ = window.center();
                        }
                    }
                }

                let window_failsafe = window.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    if let Ok(false) = window_failsafe.is_visible() {
                        log::warn!(
                            "Frontend failed to report ready within timeout. Forcing window visibility."
                        );
                        let _ = window_failsafe.show();
                        let _ = window_failsafe.set_focus();

                        // BLITZRAW: showing it small and maximising it ten
                        // seconds later when the front end finally answers is
                        // still the window opening small, as far as anyone
                        // watching is concerned. The remembered state is right
                        // here, so use it.
                        #[cfg(any(windows, target_os = "linux"))]
                        {
                            let handle = window_failsafe.app_handle().clone();
                            let saved = *handle.state::<AppState>().startup_window_state.lock().unwrap();
                            if let Some(saved) = saved {
                                if saved.maximized {
                                    let _ = window_failsafe.maximize();
                                }
                                if saved.fullscreen {
                                    let _ = window_failsafe.set_fullscreen(true);
                                }
                                log::info!(
                                    "Failsafe restored the window: maximized={} fullscreen={}",
                                    saved.maximized,
                                    saved.fullscreen
                                );
                            }
                        }
                    }
                });

                let pending_window_state = Arc::new(Mutex::new(None::<WindowState>));
                let pending_state_for_saver = pending_window_state.clone();
                let app_handle_for_saver = app.handle().clone();

                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;

                        let state_to_save = {
                            let mut lock = pending_state_for_saver.lock().unwrap();
                            lock.take()
                        };

                        // BLITZRAW: one data directory, chosen and proved.
                        if let Some(state) = state_to_save {
                            let config_dir = crate::data_dir::data_dir(&app_handle_for_saver);
                            let path = config_dir.join("window_state.json");
                            let _ = std::fs::create_dir_all(&config_dir);
                            if let Ok(json) = serde_json::to_string(&state) {
                                let _ = std::fs::write(&path, json);
                            }
                        }
                    }
                });

                let window_for_handler = window.clone();
                let pending_state_for_handler = pending_window_state.clone();

                let closing_handle = window.app_handle().clone();

                window.on_window_event(move |event| match event {
                    // ========== BLITZRAW: the panel windows go with it ==========
                    // Tauri keeps the process alive while any window is open, so
                    // closing the main window while a panel window was out left
                    // the application running with nothing to run it from, and
                    // the terminal had to be killed. The panel windows belong to
                    // this one and have nothing to show without it.
                    tauri::WindowEvent::CloseRequested { .. } => {
                        // BLITZRAW: Windows sends one last Resized as a window
                        // is destroyed, and a maximised window does not always
                        // still report itself as maximised by then. Whatever is
                        // pending now is the truth; nothing after this is.
                        closing_handle
                            .state::<AppState>()
                            .window_closing
                            .store(true, Ordering::SeqCst);
                        flush_window_state(&closing_handle, &pending_state_for_handler);
                        crate::panel_window::close_every_panel_window(&closing_handle);
                    }
                    // ======== BLITZRAW END: the panel windows go with it ========
                    // ====== BLITZRAW: the two windows travel together ======
                    // Coming back to the photo brings its panels with it. On
                    // Windows the owner relationship set up in panel_window.rs
                    // already does this; this covers the case where that could
                    // not be established, and it costs nothing when it did.
                    tauri::WindowEvent::Focused(true) => {
                        crate::panel_window::bring_panels_forward(&closing_handle);
                    }
                    // ==== BLITZRAW END: the two windows travel together ====
                    tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_) => {
                        // ==== BLITZRAW: the panels go to the other screen ====
                        // Only when the photo has actually changed screens, and
                        // only when the panels are on the same one. A move
                        // within a screen, which is most moves, gets as far as
                        // the comparison below and no further.
                        crate::window_places::keep_panels_off_the_photo(&closing_handle);
                        // == BLITZRAW END: the panels go to the other screen ==

                        // BLITZRAW: three reasons a window's own report of
                        // itself is not worth writing down. See
                        // `window_state_is_worth_saving`.
                        {
                            let state = window_for_handler.app_handle().state::<AppState>();
                            if !window_state_is_worth_saving(
                                state.window_state_restored.load(Ordering::SeqCst),
                                state.window_closing.load(Ordering::SeqCst),
                                window_for_handler.is_minimized().unwrap_or(false),
                            ) {
                                return;
                            }
                        }

                        #[cfg(any(windows, target_os = "linux"))]
                        let maximized = window_for_handler.is_maximized().unwrap_or(false);
                        #[cfg(not(any(windows, target_os = "linux")))]
                        let maximized = false;

                        #[cfg(any(windows, target_os = "linux"))]
                        let fullscreen = window_for_handler.is_fullscreen().unwrap_or(false);
                        #[cfg(not(any(windows, target_os = "linux")))]
                        let fullscreen = false;

                        let mut state = WindowState {
                            width: 1280,
                            height: 720,
                            x: 0,
                            y: 0,
                            maximized,
                            fullscreen,
                        };

                        if let Ok(position) = window_for_handler.outer_position() {
                            state.x = position.x;
                            state.y = position.y;
                        }

                        if !maximized
                            && !fullscreen
                            && let Ok(size) = window_for_handler.outer_size()
                            && size.width >= 800
                            && size.height >= 600
                        {
                            state.width = size.width;
                            state.height = size.height;
                        }

                        *pending_state_for_handler.lock().unwrap() = Some(state);
                    }
                    _ => {}
                });
            }

            // BLITZRAW: thumbnails written under the old flat layout cannot be
            // reached any more and cannot be attributed to an image either, so
            // they are cleared once. Costs one directory listing on every start
            // after that, when there is nothing left at the top level to find.
            {
                let sweeping = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    crate::file_management::sweep_legacy_thumbnails(&sweeping);
                });
            }

            crate::register_exit_handler();
            Ok(())
        })
        .manage(AppState {
            window_setup_complete: AtomicBool::new(false),
            // BLITZRAW: read once at setup, so the saver cannot overwrite the
            // answer before the restore gets to use it.
            startup_window_state: Mutex::new(None),
            window_state_restored: AtomicBool::new(false),
            window_closing: AtomicBool::new(false),
            gpu_crash_flag_path: Mutex::new(None),
            original_image: Mutex::new(None),
            cached_preview: Mutex::new(None),
            gpu_context: Mutex::new(None),
            gpu_image_cache: Mutex::new(None),
            gpu_processor: Mutex::new(None),
            ai_state: Mutex::new(None),
            ai_init_lock: TokioMutex::new(()),
            export_task_token: Arc::new(Mutex::new(None)),
            hdr_result: Arc::new(Mutex::new(None)),
            panorama_result: Arc::new(Mutex::new(None)),
            denoise_result: Arc::new(Mutex::new(None)),
            indexing_task_handle: Mutex::new(None),
            lut_cache: Mutex::new(HashMap::new()),
            initial_file_path: Mutex::new(None),
            pending_edit_session: Mutex::new(None),
            thumbnail_cancellation_token: Arc::new(AtomicBool::new(false)),
            thumbnail_progress: Mutex::new(ThumbnailProgressTracker { total: 0, completed: 0 }),
            preview_worker_tx: Mutex::new(None),
            analytics_worker_tx: Mutex::new(None),
            mask_cache: Mutex::new(HashMap::new()),
            patch_cache: Mutex::new(HashMap::new()),
            geometry_cache: Mutex::new(HashMap::new()),
            thumbnail_geometry_cache: Mutex::new(HashMap::new()),
            lens_db: Mutex::new(None),
            load_image_generation: Arc::new(AtomicUsize::new(0)),
            full_warped_cache: Mutex::new(None),
            full_transformed_cache: Mutex::new(None),
            decoded_image_cache: Mutex::new(DecodedImageCache::new(5)),
            apply_adjustments_generation: Arc::new(AtomicUsize::new(0)),
            thumbnail_manager: ThumbnailManager::new(),
            metadata_manager: MetadataManager::new(),
            disks_cache: Mutex::new(None),
            disks_cache_refreshing: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            // BLITZRAW: as-shot Kelvin and tint, for the white balance panel.
            crate::camera_profile::get_white_balance_info,
            crate::camera_profile::pick_white_balance,
            apply_adjustments,
            generate_preview_for_path,
            generate_original_transformed_preview,
            generate_preset_preview,
            generate_uncropped_preview,
            preview_geometry_transform,
            get_log_file_path,
            frontend_log,
            save_collage,
            merge_hdr,
            save_hdr,
            lut_processing::load_and_parse_lut,
            lut_processing::list_luts,
            lut_processing::import_luts,
            lut_processing::remove_lut,
            lut_processing::generate_lut_previews,
            fetch_community_presets,
            generate_all_community_previews,
            save_temp_file,
            get_image_dimensions,
            frontend_ready,
            cancel_thumbnail_generation,
            update_wgpu_transform,
            android_integration::resolve_android_content_uri_name,
            cache_utils::clear_session_caches,
            cache_utils::clear_image_caches,
            app_settings::load_settings,
            app_settings::save_settings,
            ai_commands::generate_ai_subject_mask,
            ai_commands::precompute_ai_subject_mask,
            ai_commands::generate_ai_foreground_mask,
            ai_commands::generate_ai_sky_mask,
            ai_commands::generate_ai_depth_mask,
            ai_commands::check_ai_connector_status,
            ai_commands::test_ai_connector_connection,
            ai_commands::generate_full_image_depth_map,
            inpainting::invoke_generative_replace_with_mask_def,
            inpainting::generate_manual_cleanup_patch,
            denoising::apply_denoising,
            denoising::batch_denoise_images,
            denoising::denoise_preview_patch,
            denoising::save_denoised_image,
            image_loader::load_image,
            image_loader::is_image_cached,
            panorama_stitching::stitch_panorama,
            panorama_stitching::save_panorama,
            export_processing::export_images,
            export_processing::cancel_export,
            export_processing::estimate_export_sizes,
            image_processing::calculate_auto_adjustments,
            mask_generation::generate_mask_overlay,
            file_management::update_exif_fields,
            file_management::get_supported_file_types,
            file_management::read_exif_for_paths,
            file_management::list_images_in_dir,
            file_management::list_images_recursive,
            nef_compression::probe_raw_compression,
            panel_window::open_floating_window,
            panel_window::close_floating_window,
            panel_window::panel_window_ready,
            panel_window::floating_window_is_open,
            // BLITZRAW: where the windows sit, so a layout profile can put
            // them back. See window_places.rs.
            window_places::get_window_places,
            window_places::apply_window_places,
            auto_stack::preview_auto_stacks,
            auto_stack::preview_burst_stacks,
            bulk_hdr::hdr_outputs_present,
            stacks::set_stacks,
            stacks::set_stack_leader,
            stacks::clear_stacks,
            dng_convert::find_dng_converter,
            dng_convert::convert_to_dng,
            file_management::get_folder_tree,
            file_management::get_folder_children,
            file_management::get_pinned_folder_trees,
            file_management::update_thumbnail_queue,
            file_management::create_folder,
            file_management::delete_folder,
            file_management::copy_files,
            file_management::move_files,
            file_management::rename_folder,
            file_management::rename_files,
            file_management::nudge_adjustments_for_paths,
            file_management::go_to_steps,
            preview_cache::build_previews_for_paths,
            preview_cache::discard_previews_for_paths,
            preview_cache::cached_preview_for_path,
            preview_cache::count_cached_previews,
            file_management::duplicate_file,
            file_management::show_in_finder,
            file_management::delete_files_from_disk,
            file_management::delete_files_with_associated,
            file_management::save_metadata_and_update_thumbnail,
            // BLITZRAW: pin the state an export sent out.
            file_management::pin_exported_state,
            // BLITZRAW: scopes from the preview or thumbnail, before any decode.
            file_management::scopes_from_small_picture,
            // BLITZRAW: a nudge on the small picture, while the raw decodes.
            proxy_preview::render_nudged_preview,
            proxy_preview::forget_nudged_source,
            file_management::apply_adjustments_to_paths,
            file_management::load_metadata,
            file_management::load_presets,
            file_management::save_presets,
            file_management::get_or_create_internal_library_root,
            file_management::reset_adjustments_for_paths,
            file_management::apply_auto_adjustments_to_paths,
            file_management::handle_import_presets_from_file,
            file_management::handle_import_legacy_presets_from_file,
            file_management::handle_import_presets_from_files,
            file_management::handle_export_presets_to_file,
            file_management::save_community_preset,
            file_management::clear_all_sidecars,
            file_management::clear_thumbnail_cache,
            file_management::set_color_label_for_paths,
            file_management::set_rating_for_paths,
            file_management::import_files,
            file_management::create_virtual_copy,
            file_management::get_albums,
            file_management::save_albums,
            file_management::add_to_album,
            file_management::get_album_images,
            tagging::start_background_indexing,
            tagging::clear_ai_tags,
            tagging::clear_all_tags,
            tagging::add_tag_for_paths,
            tagging::remove_tag_for_paths,
            culling::cull_images,
            lens_correction::get_lensfun_makers,
            lens_correction::get_lensfun_lenses_for_maker,
            lens_correction::autodetect_lens,
            lens_correction::get_lens_distortion_params,
            negative_conversion::preview_negative_conversion,
            negative_conversion::convert_negatives,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(#[allow(unused_variables)] |app_handle, event| {
            match event {
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Opened { urls } => {
                    if let Some(url) = urls.first()
                        && let Ok(path) = url.to_file_path()
                        && let Some(path_str) = path.to_str()
                    {
                        let state = app_handle.state::<AppState>();
                        *state.initial_file_path.lock().unwrap() = Some(path_str.to_string());
                        log::info!("macOS initial open: Stored path {} for later.", path_str);
                    }
                }
                tauri::RunEvent::ExitRequested { api, .. } => {
                    api.prevent_exit();

                    #[cfg(target_os = "macos")]
                    unsafe { libc::_exit(0); }

                    #[cfg(not(target_os = "macos"))]
                    std::process::exit(0);
                }
                tauri::RunEvent::Exit => {
                    #[cfg(target_os = "macos")]
                    unsafe { libc::_exit(0); }

                    #[cfg(not(target_os = "macos"))]
                    std::process::exit(0);
                }
                _ => {}
            }
        });
}

// ============ BLITZRAW: the window that opened small ============
#[cfg(test)]
mod window_state_tests {
    use super::window_state_is_worth_saving;

    #[test]
    fn a_settled_window_is_saved() {
        assert!(window_state_is_worth_saving(true, false, false));
    }

    #[test]
    fn nothing_is_saved_before_the_restore_has_run() {
        // The bug this whole thing exists for. Setup's own set_size and
        // set_position fire Resized and Moved while the window is still the
        // wrong size, and saving then overwrites the answer the restore is
        // about to read.
        assert!(!window_state_is_worth_saving(false, false, false));
    }

    #[test]
    fn nothing_is_saved_once_the_window_is_closing() {
        // Windows sends a last Resized as a window is destroyed, by which time
        // a maximised window may no longer say it is maximised.
        assert!(!window_state_is_worth_saving(true, true, false));
    }

    #[test]
    fn a_minimized_window_is_not_where_the_user_left_it() {
        assert!(!window_state_is_worth_saving(true, false, true));
    }
}
// ========== BLITZRAW END: the window that opened small ==========

#[cfg(test)]
mod logging_tests {
    use super::{LOGS_KEPT, log_files_oldest_first, prune_logs};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Stands in for one app start: make this session a file, then prune.
    ///
    /// Stamped rather than left to the clock, because several sessions in one
    /// tick would make the order the test asserts depend on the tiebreak alone.
    fn start_a_session(dir: &std::path::Path, index: usize, body: &str) {
        let path = dir.join(format!("app-{index:04}.log"));
        std::fs::write(&path, body).expect("write");
        filetime::set_file_mtime(
            &path,
            filetime::FileTime::from_unix_time(1_600_000_000 + index as i64, 0),
        )
        .expect("stamp");
        prune_logs(dir);
    }

    /// The log of the run worth reading has to survive the restart that follows
    /// it, and the folder has to stop growing.
    ///
    /// One file was reused and truncated at startup, so asking "how long did
    /// that bulk merge take" after reopening the app read a file describing the
    /// reopening. Twice that had to be answered out of file modification times
    /// instead.
    #[test]
    fn every_session_gets_its_own_log_and_the_oldest_are_dropped() {
        let dir = scratch("blitzraw-log-sessions");
        let sessions = LOGS_KEPT + 4;

        for session in 0..sessions {
            start_a_session(&dir, session, &format!("session {session}"));
        }

        let kept = log_files_oldest_first(&dir);
        assert_eq!(kept.len(), LOGS_KEPT, "the folder stops at {LOGS_KEPT}");

        let read = |path: &std::path::PathBuf| std::fs::read_to_string(path).expect("kept");
        assert_eq!(
            read(kept.last().expect("newest")),
            format!("session {}", sessions - 1),
            "the session that just started is there"
        );
        assert_eq!(
            read(kept.first().expect("oldest")),
            format!("session {}", sessions - LOGS_KEPT),
            "and the oldest kept is exactly {LOGS_KEPT} sessions back"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Files from the numbered scheme this replaced are pruned by the same rule
    /// rather than sitting in the folder forever.
    #[test]
    fn logs_from_the_old_naming_are_swept_up_too() {
        let dir = scratch("blitzraw-log-legacy");
        for (index, name) in ["app.log", "app.1.log", "app.2.log", "app.3.log"]
            .iter()
            .enumerate()
        {
            let path = dir.join(name);
            std::fs::write(&path, "old").expect("write");
            filetime::set_file_mtime(
                &path,
                filetime::FileTime::from_unix_time(1_500_000_000 + index as i64, 0),
            )
            .expect("stamp");
        }
        assert_eq!(log_files_oldest_first(&dir).len(), 4, "all four are seen");

        for session in 0..LOGS_KEPT {
            start_a_session(&dir, session, "new");
        }

        let kept = log_files_oldest_first(&dir);
        assert_eq!(kept.len(), LOGS_KEPT);
        assert!(
            kept.iter()
                .all(|path| std::fs::read_to_string(path).expect("kept") == "new"),
            "nothing from the old scheme is left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder with no logs is the first run ever, not an error.
    #[test]
    fn the_first_run_has_nothing_to_prune() {
        let dir = scratch("blitzraw-log-empty");
        prune_logs(&dir);
        assert!(log_files_oldest_first(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
